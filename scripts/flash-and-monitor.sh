#!/bin/sh
# The `cargo run` runner: flash, then monitor, with a timestamp on every line.
#
# `espflash` has no timestamp option, so its output goes through `ts` (from
# moreutils) when that is installed, and through `scripts/stamp.py` when it is
# not - the same preference the Makefile's `STAMP` makes, and the same
# standard-tool-with-a-fallback shape as `make size`.
#
# Wrapping the runner rather than the Makefile keeps `cargo run` working and
# leaves the target directory to cargo.
#
# The exit status is the monitor's, which is not actionable: it exits whenever
# the device deep-sleeps and the serial port goes away with it.
dir=$(dirname "$0")

if command -v ts >/dev/null 2>&1; then
	stamper=ts
	stamper_arg='%H:%M:%S'
else
	stamper=python3
	stamper_arg="$dir/stamp.py"
fi

espflash flash --monitor "$@" | "$stamper" "$stamper_arg"
