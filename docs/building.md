# Building, flashing, and the traps

## What you need

- The `nightly` toolchain with `rust-src`, pinned in `rust-toolchain.toml`.
- [`espflash`](https://github.com/esp-rs/espflash) on `PATH`, to flash.
- On macOS, a riscv-capable clang (see below).

## The targets

```sh
make build            # firmware, debug
make build-release    # firmware, release
make flash            # build + flash + monitor
make flash-release    # the same, optimised
make monitor          # attach to an already-flashed board
make size             # how big the image is, against its partition
make check            # cargo check
make fmt              # cargo fmt
```

`cargo run` uses the runner in `.cargo/config.toml`, so flashing and monitoring
happen in one step.

`espflash.toml` points at the custom `partitions.csv`: Matter's key-value store in
`nvs`, the calibration in `calib`, and a single non-OTA application partition
taking the rest of the flash.

## macOS needs a different clang

The `mbedtls` feature builds C for `riscv32imac`, and Apple's `/usr/bin/clang`
cannot target it — the build fails with `No available targets are compatible with
triple "riscv32-unknown-unknown"`. Put Homebrew LLVM first on `PATH`:

```sh
export PATH=/opt/homebrew/opt/llvm/bin:$PATH
export CC=/opt/homebrew/opt/llvm/bin/clang CXX=/opt/homebrew/opt/llvm/bin/clang++
```

Linux CI's clang supports riscv out of the box, so this is macOS-only.

## The host target must not use `build-std`

`.cargo/config.toml` enables `build-std` because the bare-metal target needs the
standard library built from source. Host commands have to clear it with
`-Zbuild-std=`, and the `Makefile` does so through its `HOST_BUILD` variable.

The reason is worth knowing, because the error message is not obvious: building
`core` from source for a target that *also* has a prebuilt `std` puts two `core`
crates in the graph, and the link fails with

```
error[E0152]: duplicate lang item in crate `core` ... `sized`
```

## Dependencies are target-gated, and that is load-bearing

Every dependency in `fw/Cargo.toml` sits under
`[target.'cfg(target_os = "none")'.dependencies]`, and its binary is
`test = false`. Do not move them back to a plain `[dependencies]`: Cargo compiles
a package's dependency set for the *target triple*, not for the selected target
kind, so a host `cargo test --lib` would then try to build `esp-hal`, whose build
script refuses a host target outright. That is what used to make `make test` and
the host lint pass fail on any machine.

`default-members = ["fw"]` keeps a bare `cargo build` pointed at the firmware.
`sim` is host-only and must never be reached by an embedded build: only it may
enable `rs-matter`'s `os` feature, which is what supplies the host time driver and
the `critical-section` implementation, and enabling that for the bare-metal target
would break the firmware.

For the same reason, do not build the firmware with `--all-targets` for the host.

## Linting

```sh
make lint
```

Three passes: the firmware, the two lower layers as they build for the device, and
those same layers as they build for the host — where the tests and the simulator
live, and where nothing device-specific may appear.

## The pinned forks

`rs-matter-embassy` is pinned to `sysgrok/rs-matter-embassy`'s `icd` branch by git
revision, and the workspace's `[patch.crates-io]` redirects `rs-matter` to
`project-chip/rs-matter`'s main branch, `openthread` to `sysgrok/openthread`'s
`csl` branch, and `esp-hal` and friends to `ivmarkov/esp-hal`'s
`ieee802154-sleep` branch.

The Thread driver's ICD support and the CSL work need APIs that only exist past
the published releases, which is why the pins exist. `.cargo/config.toml` also
pins `esp-hal` and `esp-rtos` to a specific revision; moving that revision
usually means adapting the peripheral, ADC, RNG and LEDC calls in `fw/`, which are
pre-1.0 APIs.

Retarget everything to upstream and the registry once the relevant pull requests
and their dependencies have merged.
