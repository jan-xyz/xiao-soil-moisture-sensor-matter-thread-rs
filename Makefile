.PHONY: build build-release flash flash-release monitor watch fmt lint test sim check size clean

HOST := $(shell rustc -vV | sed -n 's/host: //p')

# `-Zbuild-std` comes from `.cargo/config.toml`, where it is needed for the
# bare-metal target. Host commands have to clear it: building `core` from source
# for a target that also has a prebuilt `std` puts two `core` crates in the graph
# and fails with "duplicate lang item in crate `core`". Hence `-Zbuild-std=`.
HOST_BUILD := --target $(HOST) -Zbuild-std=

# The crates that build for the host. `fw` is deliberately not among them: it is
# the device layer, and it only builds for the bare-metal target.
HOST_CRATES := -p soil-sensor-core -p soil-sensor-node -p soil-sensor-sim

# ARGS is passed to the simulator, e.g. `make sim ARGS="--days 7 --verbose"`.
ARGS ?=

# `cargo run` uses the `runner` in `.cargo/config.toml` (`espflash flash
# --monitor`), so flashing and monitoring happen in one step.

build:
	cargo build

build-release:
	cargo build --release

flash:
	cargo run

flash-release:
	PATH="/opt/homebrew/opt/llvm/bin:$(PATH)" \
	CC=/opt/homebrew/opt/llvm/bin/clang CXX=/opt/homebrew/opt/llvm/bin/clang++ \
	cargo run --release

# A timestamp on every monitored line, in the same format either way. `ts`, from
# moreutils, is the tool for this when it is installed; `scripts/stamp.py` is the
# same thing without the extra package, the way `make size` prefers `llvm-size`
# and falls back to a script.
#
# Absolute wall-clock rather than `ts -i`'s interval since the previous line: the
# monitor drops the connection whenever the device deep-sleeps, so the gap worth
# seeing happens *between* runs and an interval counter would restart at zero in
# each one. Only a clock spans that.
STAMP := $(shell command -v ts >/dev/null 2>&1 && echo "ts '%H:%M:%S'" || echo "python3 scripts/stamp.py")

# The serial output. `espflash` has no timestamp option of its own, and a log
# that spans deep-sleep cycles needs one.
monitor:
	espflash monitor | $(STAMP)

# The monitor exits whenever the device deep-sleeps and takes the USB port with
# it, so watching a sleepy device means re-attaching over and over - and a
# freshly attached monitor always misses the boot's first log lines. `watch`
# loops the monitor instead. The useful moment is a button press: it keeps the
# device awake for `BUTTON_WAKE_HOLD` (30 s), so press, then run this - the
# re-attached monitor catches the boot from its second wake-cause line on.
#
# A retry while the device sleeps fails to find a port and would spam the
# terminal with connection attempts, so the loop runs it with stderr silenced;
# the device's own output arrives on stdout.
watch:
	@while true; do espflash monitor 2>/dev/null | $(STAMP); sleep 1; done

fmt:
	cargo fmt

lint:
	cargo clippy -- -D warnings
	cargo clippy -p soil-sensor-core -p soil-sensor-node -- -D warnings
	cargo clippy $(HOST_CRATES) $(HOST_BUILD) --all-targets -- -D warnings

test:
	cargo test -p soil-sensor-core -p soil-sensor-node $(HOST_BUILD)
	cargo test -p soil-sensor-sim $(HOST_BUILD)

sim:
	cargo run -p soil-sensor-sim --bin sim $(HOST_BUILD) -- $(ARGS)

check:
	cargo check

size:
	@python3 scripts/firmware-size.py

clean:
	cargo clean
