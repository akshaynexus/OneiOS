// SPDX-License-Identifier: MIT
//! The Qualcomm SEE ("Sensors Execution Environment") client protocol, as the Android sensors HAL and libssc speak
//! it: QMI service 400 (SNS client) over QRTR, carrying protobuf messages. Byte buffers only: no socket here
//! (qrtr.rs sends them), so every function is tested on the build host with recorded bytes (tests.rs).
//!
//! Ported from DenialOfGravity/ios-redfin 23019ab4 `phone/sensors/sns.c`, `sns.h` and `phone/wifi/qmi.c` (MIT,
//! Copyright (c) 2026 dipa-studio), message build and parse only; their own QRTR device for XNU (`rqrtr.c`) is not
//! needed: Linux has AF_QIPCRTR.
//!
//! QMI (service 400, IDL sns_client_api_v01), each message a 7-byte header (u8 type: 0 request, 2 response, 4
//! indication; u16 transaction; u16 message id; u16 length) and TLVs (u8 type, u16 length, value):
//!   0x20 SNS_CLIENT_REQ   TLV 0x01 payload (u16 length + sns_client_request_msg), TLV 0x10 use_jumbo_report (u8)
//!        response         TLV 0x02 result (u16 result, u16 error), TLV 0x10 client_id (u64)
//!   0x21 SNS_CLIENT_REPORT_IND, 0x22 SNS_CLIENT_JUMBO_REPORT_IND: TLV 0x01 client_id, TLV 0x02 payload (u16 length +
//!        sns_client_event_msg)
//! Protobuf (libssc's sns_client.proto, sns_std.proto, sns_std_sensor.proto, sns_suid.proto):
//!   sns_std_suid            { 1 fixed64 low; 2 fixed64 high }
//!   sns_client_request_msg  { 1 suid; 2 fixed32 msg_id; 3 suspend_config { 1 proc type (APSS 1); 2 delivery
//!                             (WAKEUP 0, NO_WAKEUP 1) }; 4 sns_std_request { 1 batching; 2 bytes payload } }
//!   sns_client_event_msg    { 1 suid; 2 repeated event { 1 fixed32 msg_id; 2 fixed64 timestamp; 3 bytes payload } }
//!   SUID lookup at abababab.. : 512 { 1 string data_type; 2 bool register_updates; 3 bool default_only },
//!                               768 { 1 string data_type; 2 repeated suid }
//!   1 attribute request { 1 bool register_updates } -> 128 { 1 repeated attr { 1 int32 id; 2 value { 1 repeated
//!     data { 1 subtype; 2 string; 3 float; 4 sint64; 5 bool } } } }
//!   513 sensor config { 1 float sample_rate } (streaming), 514 on-change config (empty), 10 disable (empty)
//!   1025 sensor event { 1 repeated float data; 2 enum status (0 unreliable .. 3 high) }, 130 error { 1 enum }

use super::pb::{self, Field, Writer};

pub const SERVICE: u32 = 400;
pub const CLIENT_REQ: u16 = 0x20;
pub const REPORT_IND: u16 = 0x21;
pub const JUMBO_IND: u16 = 0x22;

pub const QMI_REQUEST: u8 = 0;
pub const QMI_RESPONSE: u8 = 2;
pub const QMI_INDICATION: u8 = 4;

pub const MSG_DISABLE: u32 = 10;
pub const MSG_ATTR_REQ: u32 = 1;
pub const MSG_ATTR_EVENT: u32 = 128;
pub const MSG_FLUSH_EVENT: u32 = 129;
pub const MSG_ERROR_EVENT: u32 = 130;
pub const MSG_SUID_REQ: u32 = 512;
pub const MSG_SENSOR_CONFIG: u32 = 513;
pub const MSG_ONCHANGE_CONFIG: u32 = 514;
pub const MSG_SUID_EVENT: u32 = 768;
/// The same number from a sensor's own SUID: sns_std_sensor_physical_config_event (ignored).
pub const MSG_PHYS_CONFIG_EVENT: u32 = 768;
pub const MSG_SENSOR_EVENT: u32 = 1025;

const PROC_APSS: u64 = 1;
/// Delivered while the AP sleeps too (the HAL's choice for a foreground client).
pub const DELIVERY_WAKEUP: u64 = 0;
pub const DELIVERY_NO_WAKEUP: u64 = 1;

/// sns_std_sensor_attr_id (those read here).
pub const ATTR_NAME: i64 = 0;
pub const ATTR_VENDOR: i64 = 1;
pub const ATTR_TYPE: i64 = 2;
pub const ATTR_AVAILABLE: i64 = 3;
pub const ATTR_RATES: i64 = 6;
pub const ATTR_RESOLUTIONS: i64 = 7;
pub const ATTR_RANGES: i64 = 11;
pub const ATTR_STREAM_TYPE: i64 = 16;
pub const ATTR_HW_ID: i64 = 18;
pub const ATTR_PLACEMENT: i64 = 20;
pub const ATTR_PHYSICAL: i64 = 21;
/// sns_std_sensor_stream_type
pub const STREAM_STREAMING: i64 = 0;
pub const STREAM_ON_CHANGE: i64 = 1;

/// A sensor's 128-bit id.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Suid {
    pub low: u64,
    pub high: u64,
}

/// The lookup sensor every SUID request goes to.
pub const SUID_LOOKUP: Suid = Suid { low: 0xabab_abab_abab_abab, high: 0xabab_abab_abab_abab };

impl std::fmt::Display for Suid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:016x}{:016x}", self.high, self.low)
    }
}

// ---- QMI framing ----------------------------------------------------------------------------------------------------

/// One QMI message: the header and its TLVs.
pub fn qmi_message(kind: u8, txn: u16, id: u16, tlvs: &[(u8, &[u8])]) -> Option<Vec<u8>> {
    let mut b = vec![kind];
    b.extend_from_slice(&txn.to_le_bytes());
    b.extend_from_slice(&id.to_le_bytes());
    b.extend_from_slice(&[0, 0]);
    for (t, v) in tlvs {
        b.push(*t);
        b.extend_from_slice(&u16::try_from(v.len()).ok()?.to_le_bytes());
        b.extend_from_slice(v);
    }
    let len = u16::try_from(b.len() - 7).ok()?;
    b[5..7].copy_from_slice(&len.to_le_bytes());
    Some(b)
}

/// A received QMI message.
#[derive(Clone, Copy, Debug)]
pub struct Qmi<'a> {
    pub kind: u8,
    pub txn: u16,
    pub id: u16,
    tlv: &'a [u8],
}

impl<'a> Qmi<'a> {
    /// The header; the TLV area is cut to the length the header gives (or what came).
    pub fn parse(d: &'a [u8]) -> Option<Qmi<'a>> {
        if d.len() < 7 {
            return None;
        }
        let len = usize::from(u16::from_le_bytes([d[5], d[6]])).min(d.len() - 7);
        Some(Qmi {
            kind: d[0],
            txn: u16::from_le_bytes([d[1], d[2]]),
            id: u16::from_le_bytes([d[3], d[4]]),
            tlv: &d[7..7 + len],
        })
    }

    /// TLV `t`'s value.
    pub fn get(&self, t: u8) -> Option<&'a [u8]> {
        let mut o = 0usize;
        while o + 3 <= self.tlv.len() {
            let l = usize::from(u16::from_le_bytes([self.tlv[o + 1], self.tlv[o + 2]]));
            if o + 3 + l > self.tlv.len() {
                return None;
            }
            if self.tlv[o] == t {
                return Some(&self.tlv[o + 3..o + 3 + l]);
            }
            o += 3 + l;
        }
        None
    }
}

// ---- requests -------------------------------------------------------------------------------------------------------

fn suid_bytes(s: &Suid) -> Vec<u8> {
    Writer::new().fixed64(1, s.low).fixed64(2, s.high).done()
}

/// 512: the SUIDs of a data type.
pub fn suid_req(data_type: &str, register_updates: bool, default_only: bool) -> Vec<u8> {
    Writer::new().string(1, data_type).boolean(2, register_updates).boolean(3, default_only).done()
}

/// 513: a streaming sensor at `hz`.
pub fn sensor_config(hz: f32) -> Vec<u8> {
    Writer::new().float(1, hz).done()
}

/// 1: the attributes, once.
pub fn attr_req(register_updates: bool) -> Vec<u8> {
    Writer::new().boolean(1, register_updates).done()
}

/// sns_client_request_msg around `payload` (may be empty: the request field is sent even then).
pub fn client_request(suid: &Suid, msg_id: u32, payload: &[u8], delivery: u64) -> Vec<u8> {
    let suspend = Writer::new().varint(1, PROC_APSS).varint(2, delivery).done();
    let mut request = Writer::new();
    if !payload.is_empty() {
        request.bytes(2, payload);
    }
    let request = request.done();
    Writer::new().bytes(1, &suid_bytes(suid)).fixed32(2, msg_id).bytes(3, &suspend).bytes(4, &request).done()
}

/// The whole QMI SNS_CLIENT_REQ around a sns_client_request_msg (at most 1000 bytes: SNS_CLIENT_REQ_LEN_MAX_V01),
/// jumbo reports allowed.
pub fn qmi_request(txn: u16, req: &[u8], jumbo: bool) -> Option<Vec<u8>> {
    if req.len() > 1000 {
        return None;
    }
    let mut tlv = (req.len() as u16).to_le_bytes().to_vec();
    tlv.extend_from_slice(req);
    if jumbo {
        qmi_message(QMI_REQUEST, txn, CLIENT_REQ, &[(0x01, &tlv), (0x10, &[1])])
    } else {
        qmi_message(QMI_REQUEST, txn, CLIENT_REQ, &[(0x01, &tlv)])
    }
}

// ---- what comes back ------------------------------------------------------------------------------------------------

/// A message of the SNS service, reduced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sns<'a> {
    /// The answer to a request: result 0 is success.
    Response {
        txn: u16,
        result: u16,
        error: u16,
        client_id: Option<u64>,
    },
    /// A report: the sns_client_event_msg.
    Report {
        client_id: Option<u64>,
        payload: &'a [u8],
    },
    Other {
        kind: u8,
        id: u16,
    },
}

/// One QMI message of the SNS service; None when it is malformed.
pub fn parse(d: &[u8]) -> Option<Sns<'_>> {
    let q = Qmi::parse(d)?;
    let client_id = |t| q.get(t).filter(|v| v.len() >= 8).map(|v| u64::from_le_bytes(v[..8].try_into().unwrap()));
    match q.kind {
        QMI_RESPONSE if q.id == CLIENT_REQ => {
            let r = q.get(0x02).filter(|v| v.len() >= 4)?;
            Some(Sns::Response {
                txn: q.txn,
                result: u16::from_le_bytes([r[0], r[1]]),
                error: u16::from_le_bytes([r[2], r[3]]),
                client_id: client_id(0x10),
            })
        }
        QMI_INDICATION if q.id == REPORT_IND || q.id == JUMBO_IND => {
            let t = q.get(0x02).filter(|v| v.len() >= 2)?;
            let n = usize::from(u16::from_le_bytes([t[0], t[1]]));
            if n > t.len() - 2 {
                return None;
            }
            Some(Sns::Report { client_id: client_id(0x01), payload: &t[2..2 + n] })
        }
        kind => Some(Sns::Other { kind, id: q.id }),
    }
}

fn suid_of(p: &[u8]) -> Option<Suid> {
    let (mut s, mut got) = (Suid::default(), 0);
    for f in pb::fields(p)? {
        match (f.num, f.wire) {
            (1, pb::FIXED64) => {
                s.low = f.v;
                got |= 1
            }
            (2, pb::FIXED64) => {
                s.high = f.v;
                got |= 2
            }
            _ => {}
        }
    }
    (got == 3).then_some(s)
}

/// One event of a sns_client_event_msg.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Event<'a> {
    pub msg_id: u32,
    pub timestamp: u64,
    pub payload: &'a [u8],
}

/// A sns_client_event_msg: its SUID and its events; None when malformed.
pub fn events(p: &[u8]) -> Option<(Suid, Vec<Event<'_>>)> {
    let fields = pb::fields(p)?;
    let mut suid = Suid::default();
    for f in &fields {
        if f.num == 1 && f.wire == pb::LEN {
            suid = suid_of(f.data)?;
        }
    }
    let mut out = Vec::new();
    for f in fields.iter().filter(|f| f.num == 2 && f.wire == pb::LEN) {
        let mut e = Event { msg_id: 0, timestamp: 0, payload: &[] };
        for g in pb::fields(f.data)? {
            match (g.num, g.wire) {
                (1, pb::FIXED32) => e.msg_id = g.v as u32,
                (2, pb::FIXED64) => e.timestamp = g.v,
                (3, pb::LEN) => e.payload = g.data,
                _ => {}
            }
        }
        out.push(e);
    }
    Some((suid, out))
}

/// 768 from the lookup: the data type and its SUIDs (default first).
pub fn suid_event(p: &[u8]) -> Option<(String, Vec<Suid>)> {
    let (mut data_type, mut suids) = (String::new(), Vec::new());
    for f in pb::fields(p)? {
        match (f.num, f.wire) {
            (1, pb::LEN) => data_type = String::from_utf8_lossy(f.data).into_owned(),
            (2, pb::LEN) => suids.extend(suid_of(f.data)),
            _ => {}
        }
    }
    Some((data_type, suids))
}

/// What an attribute event says, reduced to what is logged and used.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Attrs {
    pub name: String,
    pub vendor: String,
    pub data_type: String,
    pub available: Option<bool>,
    pub stream_type: Option<i64>,
    pub physical: Option<bool>,
    pub hw_id: Option<i64>,
    pub rates: Vec<f32>,
    pub range: Option<(f32, f32)>,
    pub resolution: Option<f32>,
    /// sns_std_sensor_placement: 12 floats (a 3x4 matrix), when given
    pub placement: Vec<f32>,
    pub count: usize,
}

#[derive(Default)]
struct Value {
    text: Option<String>,
    floats: Vec<f32>,
    int: Option<i64>,
    boolean: Option<bool>,
}

fn attr_value(p: &[u8], v: &mut Value, depth: u32) -> Option<()> {
    if depth > 3 {
        return Some(());
    }
    for f in pb::fields(p)?.iter().filter(|f| f.num == 1 && f.wire == pb::LEN) {
        for g in pb::fields(f.data)? {
            match (g.num, g.wire) {
                (1, pb::LEN) => attr_value(g.data, v, depth + 1)?, // a subtype, e.g. a range [min, max]
                (2, pb::LEN) if v.text.is_none() => v.text = Some(String::from_utf8_lossy(g.data).into_owned()),
                (3, pb::FIXED32 | pb::LEN) => pb::floats(&g, &mut v.floats, 16),
                (4, pb::VARINT) if v.int.is_none() => v.int = Some(pb::zigzag(g.v)),
                (5, pb::VARINT) if v.boolean.is_none() => v.boolean = Some(g.v != 0),
                _ => {}
            }
        }
    }
    Some(())
}

/// 128: the attributes.
pub fn attr_event(p: &[u8]) -> Option<Attrs> {
    let mut a = Attrs::default();
    for f in pb::fields(p)?.iter().filter(|f| f.num == 1 && f.wire == pb::LEN) {
        let (mut id, mut v) = (-1i64, Value::default());
        for g in pb::fields(f.data)? {
            match (g.num, g.wire) {
                (1, pb::VARINT) => id = i64::from(g.v as u32 as i32),
                (2, pb::LEN) => attr_value(g.data, &mut v, 0)?,
                _ => {}
            }
        }
        a.count += 1;
        match id {
            ATTR_NAME => a.name = v.text.unwrap_or_default(),
            ATTR_VENDOR => a.vendor = v.text.unwrap_or_default(),
            ATTR_TYPE => a.data_type = v.text.unwrap_or_default(),
            ATTR_AVAILABLE => a.available = v.boolean,
            ATTR_RATES => a.rates = v.floats,
            ATTR_RANGES if v.floats.len() >= 2 => a.range = Some((v.floats[0], v.floats[1])),
            ATTR_RESOLUTIONS => a.resolution = v.floats.first().copied(),
            ATTR_STREAM_TYPE => a.stream_type = v.int,
            ATTR_PHYSICAL => a.physical = v.boolean,
            ATTR_HW_ID => a.hw_id = v.int,
            ATTR_PLACEMENT => a.placement = v.floats,
            _ => {}
        }
    }
    Some(a)
}

/// 1025: the floats and the status (accuracy 0-3); None when it is not a sns_std_sensor_event.
pub fn sensor_event(p: &[u8]) -> Option<(Vec<f32>, u32)> {
    let (mut data, mut status) = (Vec::new(), 0u32);
    for f in pb::fields(p)? {
        match (f.num, f.wire) {
            (1, pb::FIXED32 | pb::LEN) => pb::floats(&f, &mut data, 16),
            (1, _) => return None,
            (2, pb::VARINT) => status = f.v as u32,
            _ => {}
        }
    }
    Some((data, status))
}

/// 130: the error.
pub fn error_event(p: &[u8]) -> Option<u64> {
    pb::fields(p)?.iter().find(|f| f.num == 1 && f.wire == pb::VARINT).map(|f| f.v)
}

/// For the tests and sensorctl's dump: one field per line.
pub fn describe(f: &Field) -> String {
    match f.wire {
        pb::LEN => format!("{}: {} bytes", f.num, f.data.len()),
        _ => format!("{}: {:#x}", f.num, f.v),
    }
}
