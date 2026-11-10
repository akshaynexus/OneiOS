#!/usr/bin/env python3
"""Fix a copy of xnu-linux-toolchain's iig for xnu-12377's .iig files (external/ stays unchanged).
The two fixes of iOSPlus10 scripts/laurel/ios26-iig-fix.py:
1. a parameter like `const char reason[kMax]`: the name is the word before the [ (the tool took kMax);
2. iokit/Tests/TestServices has .iig files outside DriverKit: the generated .cpp includes its header by a quoted path."""
import sys

path = sys.argv[1]
text = open(path).read()
for old, new in [
    ("""        part = part.strip()
        if not part or part == 'void':
            continue
        ident = re.findall""", """        part = re.sub(r'\\[[^\\]]*\\]', '', part.strip())
        if not part or part == 'void':
            continue
        ident = re.findall"""),
    ("""           f'#include <DriverKit/{stem}.h>\\n',
""", """           f'#include "{stem}.h"\\n',
"""),
    ("""               else f'<DriverKit/{stem}.h>')
""", """               else f'"{stem}.h"')
"""),
]:
    if old not in text:
        raise SystemExit('iig-fix: iig changed: ' + old[:60])
    text = text.replace(old, new)
open(path, 'w').write(text)
