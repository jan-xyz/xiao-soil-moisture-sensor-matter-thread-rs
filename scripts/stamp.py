#!/usr/bin/env python3
"""Prefix each line of standard input with the wall-clock time it arrived,
to microseconds, matching moreutils `ts`'s `%H:%M:%.S`.

`espflash monitor` has no timestamp option, and a log that spans deep-sleep
cycles is hard to read without one: the gap between one wake-up and the next is
the whole point of watching it.

Wall-clock rather than uptime, because the device reboots on every wake-up and
the monitor drops the connection when it sleeps - so a monotonic counter would
restart, and the gap across the sleep is exactly what you want to see.
"""

import sys
import time

for line in sys.stdin:
    sys.stdout.write(f"{time.strftime('%H:%M:%S')}.{int(time.time() * 1_000_000) % 1_000_000:06d} {line}")
    sys.stdout.flush()
