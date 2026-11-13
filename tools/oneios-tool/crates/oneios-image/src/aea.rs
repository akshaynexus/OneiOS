//! imagetool.py's `aea-info FILE`: the auth-data fields of an AEA1 header (what Apple's key service is asked for).

use crate::{crash, err, py, Fail};

/// The profile number and the fields of an AEA1 header, in the header's order (of two with one key, the later
/// value at the first one's place).
pub fn aea_info(data: &[u8]) -> Result<(u32, Vec<(String, Vec<u8>)>), Fail> {
    if data.get(..4) != Some(b"AEA1") {
        return Err(err!("not an AEA1 archive"));
    }
    let word = |at: usize| data.get(at..at + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()) as usize);
    let profile = word(4).map_or(0, |w| (w & 0xff_ffff) as u32);
    let size = word(8).ok_or_else(|| crash!("the AEA header is cut short"))?;
    if 12 + size > data.len() {
        return Err(err!("the AEA header is longer than the data given"));
    }
    let (mut fields, mut at, end): (Vec<(String, Vec<u8>)>, usize, usize) = (Vec::new(), 12, 12 + size);
    while at + 4 <= end {
        let n = word(at).unwrap_or(0);
        if n < 5 || at + n > end {
            break;
        }
        // KEY, a NUL, the value
        let field = &data[at + 4..at + n];
        let cut = field.iter().position(|b| *b == 0).unwrap_or(field.len());
        let (key, value) = (ascii(&field[..cut]), field.get(cut + 1..).unwrap_or(&[]).to_vec());
        match fields.iter_mut().find(|(k, _)| *k == key) {
            Some(slot) => slot.1 = value,
            None => fields.push((key, value)),
        }
        at += n;
    }
    Ok((profile, fields))
}

/// `bytes.decode("ascii", "replace")`.
fn ascii(bytes: &[u8]) -> String {
    bytes.iter().map(|b| if b.is_ascii() { *b as char } else { '\u{fffd}' }).collect()
}

/// `aea-info FILE`: the text it prints; an archive without a key-service field is an error.
pub fn cmd_aea_info(path: &str) -> Result<String, Fail> {
    use std::io::Read;
    let mut head = Vec::new();
    std::fs::File::open(path).and_then(|f| f.take(1 << 16).read_to_end(&mut head)).map_err(|e| crash!("{}", py::os_error(&e, path)))?;
    let (profile, fields) = aea_info(&head)?;
    let mut out = format!("profile {profile}\n");
    for (key, value) in &fields {
        let text = ascii(value);
        if value.len() <= 100 {
            out += &format!("{key}: {text}\n");
        } else {
            out += &format!("{key}: {}... ({} bytes)\n", text.chars().take(60).collect::<String>(), value.len());
        }
    }
    print!("{out}");
    if !fields.iter().any(|(k, _)| k == "com.apple.wkms.fcs-key-url" || k == "com.apple.wkms.url") {
        return Err(err!("no key-service field: this archive's key cannot be fetched from Apple"));
    }
    Ok(out)
}
