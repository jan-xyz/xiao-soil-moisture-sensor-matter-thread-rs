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
make watch            # keep re-attaching: the monitor dies at every deep sleep
make size             # how big the image is, against its partition
make check            # cargo check
make fmt              # clippy --fix, then cargo fmt
```

`cargo run` uses the runner in `.cargo/config.toml`, so flashing and monitoring
happen in one step.

### Timestamps

Every monitored line is prefixed with the wall-clock time it arrived. `espflash`
has no timestamp option of its own, and a log that spans deep-sleep cycles is
hard to read without one - the gap between one wake-up and the next is the point
of watching it.

The timestamp comes from [`ts`](https://joeyh.name/code/moreutils/) when
`moreutils` is installed (`brew install moreutils`, `apt install moreutils`), and
from `scripts/stamp.py` when it is not, in the same format either way:
`HH:MM:SS.ffffff`, because the interesting moments - a button press answered, a
fabric loaded - bunch up inside one second. Both monitoring paths are stamped:
`make monitor` pipes `espflash` through it, and `cargo run` does it through the
runner wrapper `scripts/flash-and-monitor.sh` (so the `flash` targets need no
stamping of their own - piping `cargo run` again would stamp twice).

It is wall-clock rather than uptime or `ts -i`'s interval between lines, because
the monitor drops the connection every time the device sleeps and the device
reboots on every wake-up. Both the gap and the counter therefore land *between*
monitor runs, where only a clock that keeps going still means anything.

Note that the monitor exiting is not the device failing. Deep sleep powers down
the USB-Serial/JTAG peripheral, so the chip disappears from the host and
`espflash` reports a broken pipe. To follow the device across several sleep
cycles, loop it:

```sh
while true
    make monitor
    sleep 1
end
```

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

Three clippy passes: the firmware, the two lower layers as they build for the
device, and those same layers as they build for the host — where the tests and the
simulator live, and where nothing device-specific may appear. Then `cargo fmt
--check`, `cargo machete` for unused dependencies, and `cargo deny check licenses`.
The last two are installed with `cargo install` the first time.

The lint levels are in `[workspace.lints]` in `Cargo.toml`, with the pedantic
clippy group on and `warnings` denied. That last part applies to every build, not
only to `make lint`: a warning fails `make build` and `make test` too. The other
rules are in `clippy.toml`, `rustfmt.toml` and `deny.toml`. The rustfmt import
rules are nightly-only, which costs nothing here, because the bare-metal target
needs nightly anyway.

`make fmt` applies what clippy and rustfmt can fix on their own. It lowers the
lints to warnings for that run (`--cap-lints warn`), because `clippy --fix` cannot
fix code that does not compile, and it names the three firmware-side packages,
because the flags only reach the packages a command selects.

A dependency that only exists for its features or for what it links has no `use`
that `cargo machete` could see. Such a dependency is listed in its crate's
`[package.metadata.cargo-machete]`, with the reason next to it.

## The pinned forks

Nothing here comes from the registry except the chip support crates. The
workspace's `[patch.crates-io]` redirects them:

| Crate | Where it comes from | Why |
|---|---|---|
| `rs-matter-embassy` | `sysgrok/rs-matter-embassy`, `master` | the ICD work, the KV cache (PR 91) and the move of `rs-matter-stack` upstream (PR 92) are merged but unreleased |
| `rs-matter` | `project-chip/rs-matter`, `main` | the ICD and subscription work, the `Send + Sync` rework, the micro-crate split, and the MRP backoff accounting for an ICD sender's own polling interval (#638) are past the published `0.4.1` |
| `openthread` | `esp-rs/openthread`, `main` | CSL and the radio sleep API, unreleased |
| `esp-hal` and friends | `ivmarkov/esp-hal`, `ieee802154-sleep` | `Ieee802154::stop_receive` / `EspRadio::set_sleep`, so the SED receiver is off between polls |

The `esp-hal` one is the last real fork: upstream `openthread`'s ESP radio still
carries a `TODO: Upstream in esp-radio a stop_receive counterpart to the existing
start_receive`, so the sleep path depends on it. Everything else is an unreleased
commit on a project's own repository. (`rs-matter-stack` is no longer a pin at
all: upstream absorbed it. The two rs-matter pins move together -
`sysgrok/rs-matter-embassy`'s `master` tracks `project-chip/rs-matter`'s
`main`, so bumping one without the other does not compile.)

Each is pinned by revision rather than by branch, so an upstream push cannot
change what this firmware builds. Moving a pin means rebuilding and re-flashing;
`.cargo/config.toml` also pins `esp-hal` and `esp-rtos` to a specific revision,
and moving that one usually means adapting the peripheral, ADC, RNG and LEDC
calls in `fw/`, which are pre-1.0 APIs.

Retarget each to the registry as the releases land.
