//! The kernel panics of an xnuhost session log. The hosted kernel writes (0001-hosted-machine-layer.patch,
//! `hosted_panic`):
//!
//! ```text
//! [  812.104] guest     panic(cpu 4 caller 0xfffffff00b4d5210): TEXT at pc 0x000000200454c094, lr 0x0000002003ff1038 ...
//! [  812.104] guest           x28: 0x... fp:  0x... lr:  0x...  sp:  0x...
//! [  812.104] guest           pc:  0x... cpsr: 0x...  esr: 0x...  far: 0x...
//! [  812.104] guest       frame 0xfffffff00b54c094
//! ```
//!
//! `caller` and the `frame` lines are link-time addresses of the collection (`- vm_kernel_slide`); the `pc` and
//! `lr` of a fault's text and of its register dump are the running kernel's addresses. The boot's own lines say
//! where the kernel runs: `kernel: ... link base 0x...` and `memory: N MiB at 0x..., physical = virtual (slide
//! 0x...)` (`runtime/xnuhost/src/main.c`).

/// What an address of a panic block is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// `panic(cpu N caller 0x...)`: the return address of the call to panic.
    Caller,
    /// The fault's program counter (the text's `pc 0x...` or the dump's `pc:`).
    Pc,
    /// The fault's link register: a return address.
    Lr,
    /// A `frame 0x...` line: a return address.
    Frame,
    /// An address of the command line.
    Given,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Caller => "caller",
            Kind::Pc => "pc",
            Kind::Lr => "lr",
            Kind::Frame => "frame",
            Kind::Given => "address",
        }
    }

    /// The address is where a call returns to: the call itself is the instruction before it.
    pub fn is_return(self) -> bool {
        matches!(self, Kind::Caller | Kind::Lr | Kind::Frame)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub kind: Kind,
    pub addr: u64,
    /// 1-based line of the log.
    pub line: usize,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Block {
    /// 1-based line of the `panic(cpu` line.
    pub line: usize,
    pub cpu: Option<u32>,
    /// The panic's text (the first line, after the `): `).
    pub text: String,
    pub entries: Vec<Entry>,
    /// The boot's `link base`, when the log has the `kernel:` line.
    pub link_base: Option<u64>,
    /// The boot's RAM address (`memory: N MiB at 0x...`).
    pub ram_base: Option<u64>,
    /// The boot's slide as the `memory:` line prints it.
    pub slide: Option<u64>,
}

/// The hex number at the start of `text` (`0x...`), and what follows it.
fn hex_at(text: &str) -> Option<(u64, &str)> {
    let rest = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X"))?;
    let end = rest.find(|c: char| !c.is_ascii_hexdigit()).unwrap_or(rest.len());
    if end == 0 || end > 16 {
        return None;
    }
    Some((u64::from_str_radix(&rest[..end], 16).ok()?, &rest[end..]))
}

/// The hex number after `key` in `text`.
fn hex_after(text: &str, key: &str) -> Option<u64> {
    let at = text.find(key)?;
    hex_at(text[at + key.len()..].trim_start()).map(|(v, _)| v)
}

/// Every `pc` and `lr` of a line: the word, then `:`, `=` or spaces, then `0x...` (`at pc 0x..., lr 0x...`,
/// `pc:  0x...`, `pc=0x...`). `x1:` and `cpsr:` are other words.
fn registers(text: &str, out: &mut Vec<(Kind, u64)>) {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i + 2 <= bytes.len() {
        let kind = match &bytes[i..i + 2] {
            b"pc" => Some(Kind::Pc),
            b"lr" => Some(Kind::Lr),
            _ => None,
        };
        let word_start = i == 0 || !(bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_');
        if let (Some(kind), true) = (kind, word_start) {
            let rest = text[i + 2..].trim_start_matches([':', '=']).trim_start();
            // Only what directly follows the word: `pc` in running text is not a register.
            let separated = text[i + 2..].starts_with([':', '=', ' ', '\t']);
            if let (true, Some((value, _))) = (separated, hex_at(rest)) {
                out.push((kind, value));
            }
        }
        i += 1;
    }
}

/// The text of a log line after xnuhost's frame (`[seconds] guest     `), wherever the frame starts: a tool that
/// shows the log may put its own mark before it. A line without the frame is taken whole.
fn body(line: &str) -> &str {
    match line.find("] guest ") {
        Some(at) if line[..at].contains('[') => &line[at + "] guest".len()..],
        _ => line,
    }
}

/// A `frame 0x...` line: the word at the start of the text (or after blanks), the address, nothing after it.
fn frame_line(text: &str) -> Option<u64> {
    let at = text.find("frame 0x")?;
    if !text[..at].chars().all(char::is_whitespace) {
        return None;
    }
    let (addr, tail) = hex_at(&text[at + "frame ".len()..])?;
    tail.trim().is_empty().then_some(addr)
}

/// The slide of the log's last boot (its `memory:` line), for addresses given beside a log.
pub fn last_slide(text: &str) -> Option<u64> {
    text.lines().rev().find(|l| l.contains("physical = virtual")).and_then(|l| hex_after(l, "(slide "))
}

/// The panic blocks of a log, in order. A log may hold several boots; each block carries its boot's addresses.
pub fn blocks(text: &str) -> Vec<Block> {
    let mut out: Vec<Block> = Vec::new();
    let (mut link_base, mut ram_base, mut slide) = (None, None, None);
    // The open block, and how many lines since its last address.
    let mut open: Option<(Block, usize)> = None;
    let close = |open: &mut Option<(Block, usize)>, out: &mut Vec<Block>| {
        if let Some((block, _)) = open.take() {
            out.push(block);
        }
    };
    for (index, raw) in text.lines().enumerate() {
        let line = index + 1;
        let text = body(raw);
        if let Some(at) = text.find("panic(cpu ") {
            close(&mut open, &mut out);
            let head = &text[at + "panic(cpu ".len()..];
            let digits = head.find(|c: char| !c.is_ascii_digit()).unwrap_or(head.len());
            let mut block = Block { line, cpu: head[..digits].parse().ok(), link_base, ram_base, slide, ..Block::default() };
            let message = match head.find("): ") {
                Some(end) => &head[end + 3..],
                None => "",
            };
            block.text = message.trim_end().to_string();
            if let Some(caller) = hex_after(&head[..head.find(')').unwrap_or(head.len())], "caller ") {
                block.entries.push(Entry { kind: Kind::Caller, addr: caller, line });
            }
            let mut found = Vec::new();
            registers(message, &mut found);
            for (kind, addr) in found {
                block.entries.push(Entry { kind, addr, line });
            }
            open = Some((block, 0));
            continue;
        }
        if text.contains("link base 0x") && text.contains("kernel: ") {
            close(&mut open, &mut out);
            link_base = hex_after(text, "link base ");
            ram_base = None;
            slide = None;
            continue;
        }
        if text.contains("physical = virtual") {
            ram_base = hex_after(text, " MiB at ");
            slide = hex_after(text, "(slide ");
            continue;
        }
        let Some((block, quiet)) = open.as_mut() else { continue };
        let mut any = false;
        let trimmed = text.trim_start();
        if let Some(addr) = frame_line(trimmed) {
            block.entries.push(Entry { kind: Kind::Frame, addr, line });
            any = true;
        } else if trimmed.starts_with("frame ") {
            // A frame line with something after its address: cut or joined with another record. Not an address.
        } else if let Some(addr) = hex_after(trimmed, "panic while panicking, caller ") {
            block.entries.push(Entry { kind: Kind::Caller, addr, line });
            any = true;
        } else if !block.entries.iter().any(|e| e.kind == Kind::Frame) {
            // The register dump is between the panic's first line and its frames.
            let mut found = Vec::new();
            registers(trimmed, &mut found);
            for (kind, addr) in found {
                if !block.entries.iter().any(|e| e.kind == kind && e.addr == addr) {
                    block.entries.push(Entry { kind, addr, line });
                }
                any = true;
            }
        }
        if any {
            *quiet = 0;
        } else {
            *quiet += 1;
            // Other writers' lines may sit between a panic's lines; a block ends when nothing of it came for a while.
            let frames = block.entries.iter().any(|e| e.kind == Kind::Frame);
            if *quiet > if frames { 8 } else { 48 } {
                close(&mut open, &mut out);
            }
        }
    }
    close(&mut open, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOG: &str = "\
[    0.012] xnuhost I kernel: /data/local/tmp/oneios/stage/kernel.kc, collection arm64e, link base 0xfffffff007004000, 9 segments, 135 members
[    0.020] xnuhost I memory: 9728 MiB at 0x2000004000, physical = virtual (slide 0x2ff9000000); I/O aperture 0x8000000
[  812.104] guest     panic(cpu 4 caller 0xfffffff00b4d5210): Unexpected fault in kernel physical aperture at pc 0x000000200454c094, lr 0x0000002003ff1038 (saved state: 0x2012345678)
[  812.104] guest     \t  x0:  0x0000000000000001 x1:  0x0000000000000002  x2:  0x0000000000000003  x3:  0x0000000000000004
[  812.104] guest     \t  x28: 0x0000000000000000 fp:  0x00000020178a3a10  lr:  0x0000002003ff1038  sp:  0x00000020178a39f0
[  812.104] guest     \t  pc:  0x000000200454c094 cpsr: 0x60400204         esr: 0x00000000f2000001  far: 0x0000000000000000
[  812.104] guest       frame 0xfffffff00b54c094
[  812.104] guest       frame 0xfffffff00b496984
[  812.105] xnuhost I stopping: the guest panicked
";

    #[test]
    fn one_block() {
        let found = blocks(LOG);
        assert_eq!(found.len(), 1);
        let b = &found[0];
        assert_eq!((b.line, b.cpu), (3, Some(4)));
        assert_eq!((b.link_base, b.ram_base, b.slide), (Some(0xfffffff007004000), Some(0x2000004000), Some(0x2ff9000000)));
        assert!(b.text.starts_with("Unexpected fault in kernel physical aperture"));
        let got: Vec<(Kind, u64)> = b.entries.iter().map(|e| (e.kind, e.addr)).collect();
        assert_eq!(
            got,
            [
                (Kind::Caller, 0xfffffff00b4d5210),
                (Kind::Pc, 0x200454c094),
                (Kind::Lr, 0x2003ff1038),
                (Kind::Frame, 0xfffffff00b54c094),
                (Kind::Frame, 0xfffffff00b496984),
            ]
        );
        // x1, cpsr, fp, sp, far are not code addresses; the dump's pc and lr repeat the text's.
        assert_eq!(b.entries.iter().filter(|e| e.kind == Kind::Pc).count(), 1);
    }

    #[test]
    fn several_boots_and_other_forms() {
        let log = "\
[1.0] xnuhost I kernel: k, collection arm64e, link base 0xfffffff007004000, 9 segments, 2 members
[1.1] xnuhost I memory: 4096 MiB at 0x2000004000, physical = virtual (slide 0x2ff9000000); I/O aperture 0x8000000
[2.0] guest     panic(cpu 0 caller 0xfffffff007100000): Undefined kernel instruction: pc=0x20068c3c50 instr=0
[2.0] guest       frame 0xfffffff007100004
[2.0] guest     hosted: panic while panicking, caller 0xfffffff007100008
[3.0] xnuhost I kernel: k, collection arm64e, link base 0xfffffff007004000, 9 segments, 2 members
[3.1] xnuhost I memory: 4096 MiB at 0x2400004000, physical = virtual (slide 0x33f9000000); I/O aperture 0x8000000
no frame here
[4.0] guest     panic(cpu 1 caller 0xfffffff007200000): second
[4.0] guest       frame 0xfffffff007200004 trailing words
";
        let found = blocks(log);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].slide, Some(0x2ff9000000));
        assert_eq!(
            found[0].entries.iter().map(|e| (e.kind, e.addr)).collect::<Vec<_>>(),
            [(Kind::Caller, 0xfffffff007100000), (Kind::Pc, 0x20068c3c50), (Kind::Frame, 0xfffffff007100004), (Kind::Caller, 0xfffffff007100008)]
        );
        assert_eq!(found[1].slide, Some(0x33f9000000));
        // A frame line with words after its address is not a frame line.
        assert_eq!(found[1].entries.len(), 1);
        // A log with no boot lines: the panic is found, with no slide.
        let bare = blocks("panic(cpu 2 caller 0xfffffff007300000): x\n  frame 0xfffffff007300004\n");
        assert_eq!((bare.len(), bare[0].slide, bare[0].entries.len()), (1, None, 2));
    }
}
