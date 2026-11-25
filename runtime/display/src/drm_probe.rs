// SPDX-License-Identifier: MIT
//! Read-only DRM/KMS probe (`displayctl drm`, stage 3a of `docs/plan-ios-first-session.md`): inspects `/dev/dri/card0`
//! (or a named node) beside One UI without taking DRM master or changing any state. Reports driver version, dumb
//! buffer capability, universal planes / atomic support, connectors and their modes (including whether 1440x3120@120
//! is present), CRTCs, planes (type, formats, possible CRTCs), and property names/values.

/// Mode flags/types from `<drm/drm_mode.h>`.
pub const MODE_TYPE_PREFERRED: u32 = 1 << 3;

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct ModeInfo {
    pub clock: u32,
    pub hdisplay: u16,
    pub hsync_start: u16,
    pub hsync_end: u16,
    pub htotal: u16,
    pub hskew: u16,
    pub vdisplay: u16,
    pub vsync_start: u16,
    pub vsync_end: u16,
    pub vtotal: u16,
    pub vscan: u16,
    pub vrefresh: u32,
    pub flags: u32,
    pub kind: u32,
    pub name: [u8; 32],
}

/// Computes refresh rate in Hz from `vrefresh` or pixel clock and totals.
pub fn mode_hz(clock_khz: u32, htotal: u16, vtotal: u16, vrefresh: u32) -> u32 {
    if vrefresh != 0 {
        return vrefresh;
    }
    let total = htotal as u64 * vtotal as u64;
    if total == 0 {
        return 0;
    }
    ((clock_khz as u64 * 1000 + total / 2) / total) as u32
}

/// Formats a DRM `ModeInfo` as a human-readable line.
pub fn format_mode(m: &ModeInfo) -> String {
    let hz = mode_hz(m.clock, m.htotal, m.vtotal, m.vrefresh);
    let end = m.name.iter().position(|&b| b == 0).unwrap_or(m.name.len());
    let name = String::from_utf8_lossy(&m.name[..end]);
    let pref = if m.kind & MODE_TYPE_PREFERRED != 0 { " preferred" } else { "" };
    format!("{}x{}@{} ({}, clock {} kHz, flags {:#x}{})", m.hdisplay, m.vdisplay, hz, name, m.clock, m.flags, pref)
}

/// Decodes a DRM FourCC code into a 4-character string or hex fallback.
pub fn fourcc_name(fourcc: u32) -> String {
    let bytes = fourcc.to_le_bytes();
    if bytes.iter().all(|&b| b.is_ascii_graphic() || b == b' ') {
        String::from_utf8_lossy(&bytes).into_owned()
    } else {
        format!("{fourcc:#010x}")
    }
}

/// Human-readable DRM connector type name (`DRM_MODE_CONNECTOR_*`).
pub fn connector_type_name(t: u32) -> &'static str {
    match t {
        0 => "Unknown",
        1 => "VGA",
        2 => "DVII",
        3 => "DVID",
        4 => "DVIA",
        5 => "Composite",
        6 => "SVIDEO",
        7 => "LVDS",
        8 => "Component",
        9 => "9PinDIN",
        10 => "DisplayPort",
        11 => "HDMIA",
        12 => "HDMIB",
        13 => "TV",
        14 => "eDP",
        15 => "VIRTUAL",
        16 => "DSI",
        17 => "DPI",
        18 => "Writeback",
        19 => "SPI",
        20 => "USB",
        _ => "Other",
    }
}

/// Human-readable DRM connector status (`drm_mode_connection`).
pub fn connection_name(c: u32) -> &'static str {
    match c {
        1 => "connected",
        2 => "disconnected",
        _ => "unknown",
    }
}

/// Human-readable DRM plane `type` property value.
pub fn plane_type_name(v: u64) -> &'static str {
    match v {
        0 => "Overlay",
        1 => "Primary",
        2 => "Cursor",
        _ => "Unknown",
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
mod linux {
    use super::{connection_name, connector_type_name, format_mode, fourcc_name, mode_hz, plane_type_name, ModeInfo};
    use std::ffi::CString;
    use std::fmt::Write as _;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
    use xh_core::ioctl::{ioctl, iow, iowr};

    const DRM_IOCTL_VERSION: u32 = iowr(b'd', 0x00, std::mem::size_of::<Version>());
    const DRM_IOCTL_GET_CAP: u32 = iowr(b'd', 0x0c, std::mem::size_of::<GetCap>());
    const DRM_IOCTL_SET_CLIENT_CAP: u32 = iow(b'd', 0x0d, std::mem::size_of::<SetClientCap>());
    const DRM_IOCTL_MODE_GETRESOURCES: u32 = iowr(b'd', 0xa0, std::mem::size_of::<CardRes>());
    const DRM_IOCTL_MODE_GETCRTC: u32 = iowr(b'd', 0xa1, std::mem::size_of::<Crtc>());
    const DRM_IOCTL_MODE_GETCONNECTOR: u32 = iowr(b'd', 0xa7, std::mem::size_of::<Connector>());
    const DRM_IOCTL_MODE_GETPROPERTY: u32 = iowr(b'd', 0xaa, std::mem::size_of::<GetProperty>());
    const DRM_IOCTL_MODE_GETPLANERESOURCES: u32 = iowr(b'd', 0xb5, std::mem::size_of::<PlaneRes>());
    const DRM_IOCTL_MODE_GETPLANE: u32 = iowr(b'd', 0xb6, std::mem::size_of::<Plane>());
    const DRM_IOCTL_MODE_OBJ_GETPROPERTIES: u32 = iowr(b'd', 0xb9, std::mem::size_of::<ObjGetProperties>());

    const DRM_CAP_DUMB_BUFFER: u64 = 0x1;
    const DRM_CAP_TIMESTAMP_MONOTONIC: u64 = 0x6;
    const DRM_CLIENT_CAP_UNIVERSAL_PLANES: u64 = 2;
    const DRM_CLIENT_CAP_ATOMIC: u64 = 3;
    const DRM_MODE_OBJECT_CRTC: u32 = 0xcccccccc;
    const DRM_MODE_OBJECT_PLANE: u32 = 0xeeeeeeee;

    #[repr(C)]
    #[derive(Default)]
    struct Version {
        version_major: i32,
        version_minor: i32,
        version_patchlevel: i32,
        name_len: usize,
        name: u64,
        date_len: usize,
        date: u64,
        desc_len: usize,
        desc: u64,
    }

    #[repr(C)]
    #[derive(Default)]
    struct GetCap {
        capability: u64,
        value: u64,
    }

    #[repr(C)]
    #[derive(Default)]
    struct SetClientCap {
        capability: u64,
        value: u64,
    }

    #[repr(C)]
    #[derive(Default)]
    struct CardRes {
        fb_id_ptr: u64,
        crtc_id_ptr: u64,
        connector_id_ptr: u64,
        encoder_id_ptr: u64,
        count_fbs: u32,
        count_crtcs: u32,
        count_connectors: u32,
        count_encoders: u32,
        min_width: u32,
        max_width: u32,
        min_height: u32,
        max_height: u32,
    }

    #[repr(C)]
    #[derive(Default)]
    struct Connector {
        encoders_ptr: u64,
        modes_ptr: u64,
        props_ptr: u64,
        prop_values_ptr: u64,
        count_modes: u32,
        count_props: u32,
        count_encoders: u32,
        encoder_id: u32,
        connector_id: u32,
        connector_type: u32,
        connector_type_id: u32,
        connection: u32,
        mm_width: u32,
        mm_height: u32,
        subpixel: u32,
        pad: u32,
    }

    #[repr(C)]
    #[derive(Default)]
    struct Crtc {
        set_connectors_ptr: u64,
        count_connectors: u32,
        crtc_id: u32,
        fb_id: u32,
        x: u32,
        y: u32,
        gamma_size: u32,
        mode_valid: u32,
        mode: ModeInfo,
    }

    #[repr(C)]
    #[derive(Default)]
    struct PlaneRes {
        plane_id_ptr: u64,
        count_planes: u32,
    }

    #[repr(C)]
    #[derive(Default)]
    struct Plane {
        plane_id: u32,
        crtc_id: u32,
        fb_id: u32,
        possible_crtcs: u32,
        gamma_size: u32,
        count_format_types: u32,
        format_type_ptr: u64,
    }

    #[repr(C)]
    #[derive(Default)]
    struct ObjGetProperties {
        props_ptr: u64,
        prop_values_ptr: u64,
        count_props: u32,
        obj_id: u32,
        obj_type: u32,
    }

    #[repr(C)]
    struct GetProperty {
        values_ptr: u64,
        enum_blob_ptr: u64,
        prop_id: u32,
        flags: u32,
        name: [u8; 32],
        count_values: u32,
        count_enum_blobs: u32,
    }

    impl Default for GetProperty {
        fn default() -> Self {
            // SAFETY: GetProperty is plain old data; zeroing is valid.
            unsafe { std::mem::zeroed() }
        }
    }

    fn call<T>(fd: RawFd, req: u32, arg: *mut T) -> bool {
        // SAFETY: callers pass matching DRM ioctl structs and valid array pointers.
        unsafe { ioctl(fd, req, arg) }.is_ok()
    }

    fn prop_name(fd: RawFd, prop_id: u32) -> String {
        let mut p = GetProperty { prop_id, ..Default::default() };
        if !call(fd, DRM_IOCTL_MODE_GETPROPERTY, &mut p) {
            return format!("prop#{prop_id}");
        }
        let end = p.name.iter().position(|&b| b == 0).unwrap_or(p.name.len());
        String::from_utf8_lossy(&p.name[..end]).into_owned()
    }

    fn open_node(path: &str) -> Result<OwnedFd, String> {
        let c_path = CString::new(path).map_err(|_| format!("{path}: invalid path"))?;
        // SAFETY: c_path is NUL-terminated.
        let mut fd = unsafe { libc::open(c_path.as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC) };
        if fd < 0 {
            // SAFETY: c_path is NUL-terminated.
            fd = unsafe { libc::open(c_path.as_ptr(), libc::O_RDWR | libc::O_CLOEXEC) };
        }
        if fd < 0 {
            return Err(format!("{path}: {}", std::io::Error::last_os_error()));
        }
        // SAFETY: fd is a newly opened descriptor.
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }

    pub fn probe(path: &str) -> Result<String, String> {
        let fd = open_node(path)?;
        let raw = fd.as_raw_fd();
        let mut out = String::new();
        let _ = writeln!(out, "drm: node {path} (read-only, no master)");

        let mut name_buf = [0u8; 64];
        let mut date_buf = [0u8; 64];
        let mut desc_buf = [0u8; 128];
        let mut ver = Version {
            name_len: name_buf.len(),
            name: name_buf.as_mut_ptr() as u64,
            date_len: date_buf.len(),
            date: date_buf.as_mut_ptr() as u64,
            desc_len: desc_buf.len(),
            desc: desc_buf.as_mut_ptr() as u64,
            ..Default::default()
        };
        if call(raw, DRM_IOCTL_VERSION, &mut ver) {
            let n = &name_buf[..ver.name_len.min(name_buf.len())];
            let d = &date_buf[..ver.date_len.min(date_buf.len())];
            let s = &desc_buf[..ver.desc_len.min(desc_buf.len())];
            let _ = writeln!(
                out,
                "driver: {} {}.{}.{} ({}, {})",
                String::from_utf8_lossy(n).trim_end_matches('\0'),
                ver.version_major,
                ver.version_minor,
                ver.version_patchlevel,
                String::from_utf8_lossy(d).trim_end_matches('\0'),
                String::from_utf8_lossy(s).trim_end_matches('\0'),
            );
        }

        let mut cap_dumb = GetCap { capability: DRM_CAP_DUMB_BUFFER, value: 0 };
        let dumb_ok = call(raw, DRM_IOCTL_GET_CAP, &mut cap_dumb);
        let mut cap_mono = GetCap { capability: DRM_CAP_TIMESTAMP_MONOTONIC, value: 0 };
        let mono_ok = call(raw, DRM_IOCTL_GET_CAP, &mut cap_mono);
        let univ_ok = call(
            raw,
            DRM_IOCTL_SET_CLIENT_CAP,
            &mut SetClientCap { capability: DRM_CLIENT_CAP_UNIVERSAL_PLANES, value: 1 },
        );
        let atomic_ok =
            call(raw, DRM_IOCTL_SET_CLIENT_CAP, &mut SetClientCap { capability: DRM_CLIENT_CAP_ATOMIC, value: 1 });
        let _ = writeln!(
            out,
            "caps: dumb_buffer={} monotonic_ts={} universal_planes={} atomic={}",
            if dumb_ok { cap_dumb.value } else { 0 },
            if mono_ok { cap_mono.value } else { 0 },
            univ_ok as u8,
            atomic_ok as u8
        );

        let mut crtcs = [0u32; 32];
        let mut connectors = [0u32; 32];
        let mut encoders = [0u32; 32];
        let mut res = CardRes {
            crtc_id_ptr: crtcs.as_mut_ptr() as u64,
            count_crtcs: crtcs.len() as u32,
            connector_id_ptr: connectors.as_mut_ptr() as u64,
            count_connectors: connectors.len() as u32,
            encoder_id_ptr: encoders.as_mut_ptr() as u64,
            count_encoders: encoders.len() as u32,
            ..Default::default()
        };
        if !call(raw, DRM_IOCTL_MODE_GETRESOURCES, &mut res) {
            return Err(format!("{path}: GETRESOURCES: {}", std::io::Error::last_os_error()));
        }
        let _ = writeln!(
            out,
            "resources: crtcs={} connectors={} encoders={} min={}x{} max={}x{}",
            res.count_crtcs,
            res.count_connectors,
            res.count_encoders,
            res.min_width,
            res.min_height,
            res.max_width,
            res.max_height
        );

        for &id in connectors.iter().take((res.count_connectors as usize).min(connectors.len())) {
            let mut modes = vec![ModeInfo::default(); 256];
            let mut props = [0u32; 64];
            let mut prop_vals = [0u64; 64];
            let mut conn_encs = [0u32; 16];
            let mut conn = Connector {
                connector_id: id,
                modes_ptr: modes.as_mut_ptr() as u64,
                count_modes: modes.len() as u32,
                props_ptr: props.as_mut_ptr() as u64,
                prop_values_ptr: prop_vals.as_mut_ptr() as u64,
                count_props: props.len() as u32,
                encoders_ptr: conn_encs.as_mut_ptr() as u64,
                count_encoders: conn_encs.len() as u32,
                ..Default::default()
            };
            if !call(raw, DRM_IOCTL_MODE_GETCONNECTOR, &mut conn) {
                continue;
            }
            let n_modes = (conn.count_modes as usize).min(modes.len());
            let has_1440_120 = modes[..n_modes].iter().any(|m| {
                m.hdisplay == 1440 && m.vdisplay == 3120 && mode_hz(m.clock, m.htotal, m.vtotal, m.vrefresh) == 120
            });
            let _ = writeln!(
                out,
                "connector {}: {}-{} {} {}x{}mm encoder={} modes={} (1440x3120@120={})",
                conn.connector_id,
                connector_type_name(conn.connector_type),
                conn.connector_type_id,
                connection_name(conn.connection),
                conn.mm_width,
                conn.mm_height,
                conn.encoder_id,
                conn.count_modes,
                if has_1440_120 { "yes" } else { "no" }
            );
            for m in &modes[..n_modes] {
                let _ = writeln!(out, "  mode: {}", format_mode(m));
            }
            let n_props = (conn.count_props as usize).min(props.len());
            if n_props > 0 {
                let names: Vec<String> =
                    (0..n_props).map(|i| format!("{}={}", prop_name(raw, props[i]), prop_vals[i])).collect();
                let _ = writeln!(out, "  props: {}", names.join(", "));
            }
        }

        for &id in crtcs.iter().take((res.count_crtcs as usize).min(crtcs.len())) {
            let mut crtc = Crtc { crtc_id: id, ..Default::default() };
            if !call(raw, DRM_IOCTL_MODE_GETCRTC, &mut crtc) {
                continue;
            }
            let mode_str = if crtc.mode_valid != 0 { format_mode(&crtc.mode) } else { "none".into() };
            let _ = writeln!(
                out,
                "crtc {}: fb={} mode_valid={} mode={}",
                crtc.crtc_id, crtc.fb_id, crtc.mode_valid, mode_str
            );
            let mut props = [0u32; 64];
            let mut prop_vals = [0u64; 64];
            let mut obj = ObjGetProperties {
                obj_id: id,
                obj_type: DRM_MODE_OBJECT_CRTC,
                props_ptr: props.as_mut_ptr() as u64,
                prop_values_ptr: prop_vals.as_mut_ptr() as u64,
                count_props: props.len() as u32,
            };
            if call(raw, DRM_IOCTL_MODE_OBJ_GETPROPERTIES, &mut obj) {
                let n = (obj.count_props as usize).min(props.len());
                if n > 0 {
                    let names: Vec<String> =
                        (0..n).map(|i| format!("{}={}", prop_name(raw, props[i]), prop_vals[i])).collect();
                    let _ = writeln!(out, "  props: {}", names.join(", "));
                }
            }
        }

        let mut plane_ids = [0u32; 64];
        let mut pres = PlaneRes { plane_id_ptr: plane_ids.as_mut_ptr() as u64, count_planes: plane_ids.len() as u32 };
        if call(raw, DRM_IOCTL_MODE_GETPLANERESOURCES, &mut pres) {
            let _ = writeln!(out, "planes: {}", pres.count_planes);
            for &pid in plane_ids.iter().take((pres.count_planes as usize).min(plane_ids.len())) {
                let mut fmts = [0u32; 64];
                let mut plane = Plane {
                    plane_id: pid,
                    format_type_ptr: fmts.as_mut_ptr() as u64,
                    count_format_types: fmts.len() as u32,
                    ..Default::default()
                };
                if !call(raw, DRM_IOCTL_MODE_GETPLANE, &mut plane) {
                    continue;
                }
                let mut props = [0u32; 64];
                let mut prop_vals = [0u64; 64];
                let mut obj = ObjGetProperties {
                    obj_id: pid,
                    obj_type: DRM_MODE_OBJECT_PLANE,
                    props_ptr: props.as_mut_ptr() as u64,
                    prop_values_ptr: prop_vals.as_mut_ptr() as u64,
                    count_props: props.len() as u32,
                };
                let mut ptype = "Unknown";
                let mut prop_list = Vec::new();
                if call(raw, DRM_IOCTL_MODE_OBJ_GETPROPERTIES, &mut obj) {
                    let n = (obj.count_props as usize).min(props.len());
                    for i in 0..n {
                        let name = prop_name(raw, props[i]);
                        if name == "type" {
                            ptype = plane_type_name(prop_vals[i]);
                        }
                        prop_list.push(format!("{name}={}", prop_vals[i]));
                    }
                }
                let nf = (plane.count_format_types as usize).min(fmts.len());
                let fmt_names: Vec<String> = fmts[..nf].iter().map(|&f| fourcc_name(f)).collect();
                let _ = writeln!(
                    out,
                    "plane {}: type={} crtc={} fb={} possible_crtcs={:#x} formats=[{}]",
                    plane.plane_id,
                    ptype,
                    plane.crtc_id,
                    plane.fb_id,
                    plane.possible_crtcs,
                    fmt_names.join(",")
                );
                if !prop_list.is_empty() {
                    let _ = writeln!(out, "  props: {}", prop_list.join(", "));
                }
            }
        }
        Ok(out)
    }
}

/// Probes the DRM/KMS node at `path` (read-only, no master) and returns a formatted report.
pub fn probe(path: &str) -> Result<String, String> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        linux::probe(path)
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    {
        let _ = path;
        Err("displayctl drm requires Linux or Android".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_formatting_and_hz() {
        assert_eq!(mode_hz(555_000, 1500, 3150, 0), 117);
        assert_eq!(mode_hz(0, 0, 0, 120), 120);
        let mut name = [0u8; 32];
        name[..9].copy_from_slice(b"1440x3120");
        let m = ModeInfo {
            clock: 540_000,
            hdisplay: 1440,
            vdisplay: 3120,
            vrefresh: 120,
            kind: MODE_TYPE_PREFERRED,
            name,
            ..Default::default()
        };
        let line = format_mode(&m);
        assert!(line.contains("1440x3120@120"));
        assert!(line.contains("preferred"));
    }

    #[test]
    fn names_and_fourcc() {
        assert_eq!(fourcc_name(0x3432_5258), "XR24");
        assert_eq!(fourcc_name(0x3432_5241), "AR24");
        assert_eq!(connector_type_name(16), "DSI");
        assert_eq!(connector_type_name(15), "VIRTUAL");
        assert_eq!(connection_name(1), "connected");
        assert_eq!(plane_type_name(0), "Overlay");
        assert_eq!(plane_type_name(1), "Primary");
        assert_eq!(plane_type_name(2), "Cursor");
        assert_eq!(std::mem::size_of::<ModeInfo>(), 68);
    }
}
