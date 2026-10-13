#!/usr/bin/env python3
"""Report the size of the firmware image cargo just built.

The refactor that made the layering explicit must not quietly grow the firmware,
and nothing else in the build would notice. Run it through `make size`.

`text` and `data` are what occupies flash; `bss` is RAM. The ELF on disk is much
larger than either because of debug information, which is why this asks a size
tool rather than `stat`.
"""

import json
import os
import shutil
import subprocess
import sys

APP_PARTITION_BYTES = 0x3E0000


def built_executable() -> str:
    build = subprocess.run(
        ["cargo", "build", "--release", "--message-format=json"],
        capture_output=True,
        text=True,
    )
    if build.returncode != 0:
        sys.stderr.write(build.stderr)
        raise SystemExit(build.returncode)

    executables = []
    for line in build.stdout.splitlines():
        if not line.startswith("{"):
            continue
        try:
            message = json.loads(line)
        except json.JSONDecodeError:
            continue
        if message.get("executable"):
            executables.append(message["executable"])

    if not executables:
        print("no executable produced", file=sys.stderr)
        raise SystemExit(1)

    return executables[-1]


def main() -> int:
    path = built_executable()
    print(f"firmware  {path}")

    size_tool = shutil.which("llvm-size") or shutil.which("size")
    if size_tool:
        result = subprocess.run([size_tool, path], capture_output=True, text=True)
        if result.returncode == 0:
            print(result.stdout.rstrip())
            for line in result.stdout.splitlines()[1:]:
                fields = line.split()
                if len(fields) >= 4 and fields[0].isdigit():
                    text, data = int(fields[0]), int(fields[1])
                    flash = text + data
                    print(
                        f"flash     {flash} bytes ({flash / 1024:.0f} KiB) "
                        f"of {APP_PARTITION_BYTES / 1024:.0f} KiB app partition "
                        f"({100 * flash / APP_PARTITION_BYTES:.1f}%)"
                    )
                    return 0

    size = os.path.getsize(path)
    print(f"file      {size} bytes ({size / 1024:.0f} KiB), including debug info")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
