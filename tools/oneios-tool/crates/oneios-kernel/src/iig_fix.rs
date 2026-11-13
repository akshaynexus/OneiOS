//! `oneios-tool kernel iig-fix PATH` (was kernel/linux/iig-fix.py, retired 2026-11-13): fix a copy of
//! xnu-linux-toolchain's iig for xnu-12377's .iig files (external/ stays unchanged). iig is that toolchain's own
//! Python program; kernel/linux/unixnu-layout.sh copies it and runs this on the copy.
//! The two fixes of iOSPlus10 scripts/laurel/ios26-iig-fix.py:
//! 1. a parameter like `const char reason[kMax]`: the name is the word before the [ (the tool took kMax);
//! 2. iokit/Tests/TestServices has .iig files outside DriverKit: the generated .cpp includes its header by a
//!    quoted path.

use std::path::Path;

use anyhow::{bail, Context, Result};

/// (the text of iig, what it becomes). Each must be there: a changed iig is refused, not half fixed.
const EDITS: [(&str, &str); 3] = [
    (
        "        part = part.strip()\n        if not part or part == 'void':\n            continue\n        ident = re.findall",
        "        part = re.sub(r'\\[[^\\]]*\\]', '', part.strip())\n        if not part or part == 'void':\n            continue\n        ident = re.findall",
    ),
    ("           f'#include <DriverKit/{stem}.h>\\n',\n", "           f'#include \"{stem}.h\"\\n',\n"),
    ("               else f'<DriverKit/{stem}.h>')\n", "               else f'\"{stem}.h\"')\n"),
];

pub fn fix(text: &str) -> Result<String> {
    let mut text = text.to_string();
    for (old, new) in EDITS {
        if !text.contains(old) {
            bail!("iig-fix: iig changed: {}", old.chars().take(60).collect::<String>());
        }
        text = text.replace(old, new);
    }
    Ok(text)
}

pub fn main(path: &Path) -> Result<()> {
    let text = std::fs::read_to_string(path).with_context(|| path.display().to_string())?;
    std::fs::write(path, fix(&text)?).with_context(|| path.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fixed_copy_is_refused() {
        let iig: String = EDITS.iter().map(|(old, _)| format!("x\n{old}y\n")).collect();
        let fixed = fix(&iig).unwrap();
        assert!(fixed.contains("re.sub(r'\\[[^\\]]*\\]', '', part.strip())") && fixed.contains("f'#include \"{stem}.h\"\\n',"));
        assert!(fix(&fixed).unwrap_err().to_string().starts_with("iig-fix: iig changed:         part = part.strip()"));
    }
}
