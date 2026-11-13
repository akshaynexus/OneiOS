#!/usr/bin/env python3
"""drivers/build.py's compile command for a record, as a JSON list: what `oneios-tool kernel compile-command` must
print (crates/oneios-kernel/src/drivers_build.rs is a second copy of that rule; drivers/build.py stays).

  drivers-command.py DRIVERS_BUILD_PY RECORDS NAME SOURCE OBJECT
"""
import importlib.util
import json
import sys
from pathlib import Path

build, records, name, source, target = sys.argv[1:]
spec = importlib.util.spec_from_file_location('oneios_drivers_build', build)
drivers = importlib.util.module_from_spec(spec)
spec.loader.exec_module(drivers)
print(json.dumps(drivers.command(drivers.record(Path(records), name), Path(source), Path(target))))
