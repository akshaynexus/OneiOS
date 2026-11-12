//! `image im4p IN OUT`: the payload of an IM4P (imagetool.py's cmd_im4p). DER by `simple_asn1`, LZFSE by
//! `lzfse_rust`: no `lzfse` program is needed, which imagetool.py starts for this.
//!
//! What differs from Python: a file that is not valid DER ends with "not an IM4P" (Python walks only the first
//! four items and does not look at the rest).

use crate::{crash, err, Fail};
use simple_asn1::ASN1Block;

/// (type, payload) of an IM4P: DER SEQUENCE { "IM4P", type, description, data, ... }.
pub fn payload(data: &[u8]) -> Result<(String, Vec<u8>), Fail> {
    let not = || err!("not an IM4P");
    let blocks = simple_asn1::from_der(data).map_err(|_| not())?;
    let Some(ASN1Block::Sequence(_, items)) = blocks.into_iter().next() else { return Err(not()) };
    let text = |block: &ASN1Block| match block {
        ASN1Block::IA5String(_, s) | ASN1Block::UTF8String(_, s) | ASN1Block::PrintableString(_, s) => Some(s.clone()),
        _ => None,
    };
    if items.len() < 4 || items.first().and_then(text).as_deref() != Some("IM4P") {
        return Err(not());
    }
    let kind = text(&items[1]).ok_or_else(not)?;
    match items.into_iter().nth(3) {
        Some(ASN1Block::OctetString(_, bytes)) => Ok((kind, bytes)),
        _ => Err(not()),
    }
}

/// The payload as the kernel's tools read it: LZFSE decoded, the arm64 slice of a fat file.
pub fn decode(data: &[u8]) -> Result<(String, Vec<u8>), Fail> {
    let (kind, mut payload) = payload(data)?;
    if matches!(payload.get(..4), Some(b"bvx2" | b"bvx1" | b"bvxn" | b"bvx-")) {
        let mut plain = Vec::new();
        lzfse_rust::decode_bytes(&payload, &mut plain).map_err(|e| err!("the payload is LZFSE and does not decode: {e}"))?;
        payload = plain;
    } else if payload.get(..8) == Some(b"complzss") {
        return Err(err!("the payload is LZSS, which this tool does not decode"));
    }
    if payload.get(..4) == Some(b"\xca\xfe\xba\xbe") {
        // fat: keep the arm64 slice
        let word = |at: usize| payload.get(at..at + 4).map(|b| u32::from_be_bytes(b.try_into().unwrap()));
        let count = word(4).ok_or_else(|| crash!("a fat header that ends early"))? as usize;
        let mut arm64 = Vec::new();
        for i in 0..count {
            let at = 8 + 20 * i;
            let (cpu, offset, size) = (word(at), word(at + 8), word(at + 12));
            let (Some(cpu), Some(offset), Some(size)) = (cpu, offset, size) else { return Err(crash!("a fat header that ends early")) };
            if cpu == 0x0100_000C {
                arm64.push((offset as usize, size as usize));
            }
        }
        if arm64.len() != 1 {
            return Err(err!("expected one arm64 slice"));
        }
        let (offset, size) = arm64[0];
        // Python's slice: cut at the end of the data
        let end = (offset + size).min(payload.len());
        payload = payload[offset.min(end)..end].to_vec();
    }
    Ok((kind, payload))
}

/// `im4p IN OUT`: writes OUT and returns the line Python prints.
pub fn cmd(input: &str, out: &str) -> Result<String, Fail> {
    let data = std::fs::read(input).map_err(|e| crash!("{}", crate::py::os_error(&e, input)))?;
    let (kind, payload) = decode(&data)?;
    std::fs::write(out, &payload).map_err(|e| crash!("{}", crate::py::os_error(&e, out)))?;
    Ok(format!("{kind} {} bytes sha256 {} -> {out}", payload.len(), oneios_common::sha256_hex(&payload)))
}
