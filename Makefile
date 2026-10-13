.PHONY: build build-release flash flash-release monitor fmt lint test sim check size clean

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

monitor:
	espflash monitor

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
