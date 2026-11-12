#!/usr/bin/env python3
"""`oneios-tool ioslog show` filters against the same selection made here from the unfiltered CSV.

    filters.py TOOL SOURCE REFERENCE.csv WORKDIR

REFERENCE.csv: unifiedlog_iterator's CSV of SOURCE's log (all entries). For each filter this runs
`TOOL ioslog show SOURCE -f csv FILTER...`, parses its CSV, and compares the rows, in order, with the rows of
REFERENCE.csv that the filter's rule (written here in Python, from the CSV's columns) keeps. Prints one line per
filter; exit 1 when one differs. Called by compare.sh.
"""
import collections
import csv
import hashlib
import re
import subprocess
import sys

csv.field_size_limit(1 << 30)
tool, source, reference, work = sys.argv[1:5]
COL = {name: i for i, name in enumerate(
    ["Timestamp", "Event Type", "Log Type", "Subsystem", "Thread ID", "PID", "EUID", "Persona ID", "Library",
     "Library UUID", "Activity ID", "Parent Activity ID", "Category", "Process", "Process UUID", "Message",
     "Raw Message", "Boot UUID", "System Timezone Name"])}


def rows(path):
    with open(path, newline="", encoding="utf-8") as f:
        reader = csv.reader(f)
        header = next(reader)
        assert header == list(COL), header
        yield from reader


# One pass for what the filters are made of: the commonest process, subsystem, category and pid, a time in the
# middle, the boots.
count = collections.Counter()
stamps, boots = [], collections.OrderedDict()
for row in rows(reference):
    count["proc", row[COL["Process"]].rsplit("/", 1)[-1]] += 1
    count["sub", row[COL["Subsystem"]]] += 1
    count["cat", row[COL["Category"]]] += 1
    count["pid", row[COL["PID"]]] += 1
    if len(stamps) < 2_000_000:
        stamps.append(row[COL["Timestamp"]])
    boots[row[COL["Boot UUID"]]] = None
total = sum(v for (k, _), v in count.items() if k == "proc")


def top(kind):
    best = [(v, name) for (k, name), v in count.items() if k == kind and name]
    return max(best)[1] if best else None


stamps.sort()
middle = stamps[len(stamps) // 2]
later = stamps[min(len(stamps) - 1, len(stamps) // 2 + len(stamps) // 10)]
proc, sub, cat, pid = top("proc"), top("sub"), top("cat"), top("pid")
# the boot `ioslog boots` lists last is what --boot last must choose
listed = [line.split()[0] for line in open(f"{work}/boots.txt") if re.match(r"^[0-9A-F]{32}\s", line)]
last_boot = listed[-1] if listed else None

cases = [
    ("--proc " + proc, ["--proc", proc], lambda r: r[COL["Process"]].rsplit("/", 1)[-1] == proc),
    ("--type error,fault", ["--type", "error,fault"], lambda r: r[COL["Log Type"]] in ("Error", "Fault")),
    ("--grep", ["--grep", r"(?i)fail(ed|ure)?\b.*\d"], lambda r: re.search(r"(?i)fail(ed|ure)?\b.*\d", r[COL["Message"]]) is not None),
    ("--since/--until", ["--since", middle, "--until", later], lambda r: middle <= r[COL["Timestamp"]] < later),
    ("--pid " + pid, ["--pid", pid], lambda r: r[COL["PID"]] == pid),
]
if sub:
    prefix = sub[:max(1, len(sub) * 2 // 3)]
    cases.append(("--subsystem " + prefix, ["--subsystem", prefix], lambda r: r[COL["Subsystem"]].startswith(prefix)))
if cat:
    cases.append(("--category " + cat, ["--category", cat], lambda r: r[COL["Category"]] == cat))
if last_boot:
    cases.append(("--boot last", ["--boot", "last"], lambda r: r[COL["Boot UUID"]] == last_boot))
    cases.append(("--boot " + last_boot[:8], ["--boot", last_boot[:8]], lambda r: r[COL["Boot UUID"]] == last_boot))
    if proc:
        cases.append(("--boot last --proc --type", ["--boot", "last", "--proc", proc, "--type", "default"],
                      lambda r: r[COL["Boot UUID"]] == last_boot and r[COL["Process"]].rsplit("/", 1)[-1] == proc
                      and r[COL["Log Type"]] == "Default"))


def digest(selected):
    h, n = hashlib.sha256(), 0
    for row in selected:
        h.update("\x1f".join(row).encode() + b"\x1e")
        n += 1
    return n, h.hexdigest()


# what each rule keeps of the reference, in one more pass
want = [[0, hashlib.sha256()] for _ in cases]
for row in rows(reference):
    joined = "\x1f".join(row).encode() + b"\x1e"
    for i, (_, _, rule) in enumerate(cases):
        if rule(row):
            want[i][0] += 1
            want[i][1].update(joined)

status = 0
print(f"{total} entries, {len(boots)} boots in the reference CSV")
for i, (name, args, _) in enumerate(cases):
    out = f"{work}/filter.csv"
    run = subprocess.run([tool, "ioslog", "show", source, "-f", "csv", "-o", out] + args, capture_output=True, text=True)
    if run.returncode != 0:
        print(f"FAIL   {name}: status {run.returncode}: {run.stderr.strip()[:200]}")
        status = 1
        continue
    got = digest(rows(out))
    expected = (want[i][0], want[i][1].hexdigest())
    if got == expected:
        print(f"ok     {name}: {got[0]} rows, the same as the rule keeps")
    else:
        print(f"FAIL   {name}: ioslog wrote {got[0]} rows, the rule keeps {expected[0]}")
        status = 1

# --sort: the same rows as without it, in time order; text: one entry per kept row
out = f"{work}/filter.csv"
run = subprocess.run([tool, "ioslog", "show", source, "-f", "csv", "-o", out, "--sort", "--proc", proc], capture_output=True, text=True)
got = list(rows(out))
plain = [r for r in rows(reference) if r[COL["Process"]].rsplit("/", 1)[-1] == proc]
if run.returncode == 0 and sorted(map(tuple, got)) == sorted(map(tuple, plain)) \
        and all(a[0] <= b[0] for a, b in zip(got, got[1:])):
    print(f"ok     --sort: {len(got)} rows, the unsorted set, times ascending")
else:
    print(f"FAIL   --sort: status {run.returncode}, {len(got)} rows against {len(plain)}")
    status = 1
run = subprocess.run([tool, "ioslog", "show", source, "--proc", proc, "--type", "error,fault"], capture_output=True)
want_n = sum(1 for r in plain if r[COL["Log Type"]] in ("Error", "Fault"))
starts = len(re.findall(rb"(?m)^\d{4}-\d\d-\d\d \d\d:\d\d:\d\d\.\d{6}Z \S", run.stdout))
if run.returncode == 0 and starts >= want_n and (want_n == 0 or starts > 0):
    print(f"ok     text: {want_n} entries, {starts} lines that start with a time (a message may hold such a line)")
else:
    print(f"FAIL   text: status {run.returncode}, {starts} entry lines for {want_n} entries")
    status = 1
run = subprocess.run([tool, "ioslog", "checks", source, "--boot", "all"], capture_output=True, text=True)
if run.returncode == 0 and "entries" in run.stdout:
    print("ok     checks: " + run.stdout.splitlines()[0])
else:
    print(f"FAIL   checks: status {run.returncode}: {run.stderr.strip()[:200]}")
    status = 1
sys.exit(status)
