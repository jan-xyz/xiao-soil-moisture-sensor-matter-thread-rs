.PHONY: build build-release flash flash-release monitor fmt lint test check clean

build:
	cargo build

build-release:
	cargo build --release

# `cargo run` uses the `runner` in .cargo/config.toml (`espflash flash --monitor`),
# so flashing and monitoring happen in one step.
flash:
	cargo run

flash-release:
	cargo run --release

# Attach to an already-flashed board without reflashing.
monitor:
	espflash monitor

HOST := $(shell rustc -vV | sed -n 's/host: //p')

fmt:
	cargo fmt

# Two passes: the firmware (embedded target, no_std, no test harness) and
# the host-testable lib crate's tests (host target, std available there
# because `soil_sensor_core` is only `no_std` when `cfg(test)` is off).
lint:
	cargo clippy -- -D warnings
	cargo clippy --target $(HOST) --lib --tests -- -D warnings

test:
	cargo test --target $(HOST) --lib

check:
	cargo check

clean:
	cargo clean
