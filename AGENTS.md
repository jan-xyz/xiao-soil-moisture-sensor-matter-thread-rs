# AGENTS.md

Firmware for the Seeed XIAO ESP32-C6 soil-moisture kit: a Rust `no_std` / embassy
/ `esp-hal` Matter-over-Thread accessory, ported from the C++/ESP-IDF original and
built on `rs-matter-embassy`'s `light_thread_coex` example.

**This file is the agent delta, not the documentation.** Everything worth reading
lives where a person would look for it:

| Where | What |
|---|---|
| [`README.md`](README.md) | what it is, features, power summary, limitations, commissioning |
| [`ARCHITECTURE.md`](ARCHITECTURE.md) | the layering idea, and where a change belongs |
| [`docs/design.md`](docs/design.md) | how this firmware is put together |
| [`docs/hardware.md`](docs/hardware.md) | the board, and the constraints that bite |
| [`docs/matter.md`](docs/matter.md) | the Matter side, and what the radio costs |
| [`docs/battery.md`](docs/battery.md) | how the charge and the time remaining are estimated |
| [`docs/testing.md`](docs/testing.md) | tests, the simulator, and what is deliberately untested |
| [`docs/building.md`](docs/building.md) | toolchain, flashing, platform traps |

If you learn something that a human contributor would want, put it in one of
those and link it from here. Do not let knowledge accumulate only in this file.

## Commands

Prefer the `Makefile` targets over raw `cargo`; several need flags that are easy
to get wrong by hand.

| Command | Purpose |
|---|---|
| `make build` / `build-release` | firmware, for the bare-metal target |
| `make flash` / `flash-release` / `monitor` | via `espflash` |
| `make test` | the three host suites |
| `make sim` | the behaviour, against a scripted world (`ARGS="--days 7 -v"`) |
| `make lint` | three `cargo clippy` passes |
| `make fmt`, `make check`, `make size` | as named |

## Layout

```
Cargo.toml         workspace root: members, default-members, [patch], profiles
ARCHITECTURE.md    the layering, and where a change belongs
docs/              the human-readable documentation
core/              decision layer   - behaviour, no hardware, no protocol, no clock
node/              integration layer - the Matter node
sim/               simulation layer - host only
fw/                device layer + composition root
scripts/           the firmware-size helper behind `make size`
```

## Rules that break something silently

- **Never let a test hang.** The simulator drives the boot loop with
  `embassy_futures::block_on`, which busy-polls rather than waiting, so a loop
  that fails to make progress burns a core at ~400% forever instead of blocking.
  The simulator asserts its own progress and its own virtual-time budget; if one
  of those fires, the fault is in the boot loop, not in the world.
- **`Ports::next_event` must be called with a deadline strictly in the future.**
  `App::next_deadline` guarantees it and the simulator asserts it. A deadline that
  has already passed is a wait nothing can end.
- **`App::new` takes the calibration as an argument on purpose.** A boot that
  measured against the firmware's built-in references instead of the ones a person
  captured is wrong in a way that looks like a sensor fault. That was a real bug.
- **Host commands must clear `build-std`** (`-Zbuild-std=`, via the Makefile's
  `HOST_BUILD`). Building `core` from source for a target that also has a prebuilt
  `std` fails with a duplicate-lang-item error. Details in `docs/building.md`.
- **The dependencies are target-gated, and `sim` is host-only.** Do not move
  `fw`'s dependencies back to a plain `[dependencies]`, do not add `sim` to
  `default-members`, and never build the firmware with `--all-targets` for the
  host. Only `sim` may enable `rs-matter`'s `os` feature; enabling it for the
  bare-metal target breaks the firmware.
- **The button must stay on GPIO0..7.** Deep sleep powers down the GPIO
  peripheral, and only those pads have the low-power path that survives it. The
  module's BOOT pin is GPIO9 and cannot wake this firmware.
- **The handler chain order is load-bearing.** `EmptyHandler::chain` wraps LIFO:
  the broad "any cluster on endpoint 0" matcher must be chained *first* so that it
  is evaluated last, or it swallows the ICD Management cluster.
- **Every flash operation completes inside the poll of the task that started
  it.** The boot loop and the Matter stack share one flash and one Matter
  store, and a `block_on` that waits for a lock held by an unpolled task can
  never finish on this single-core executor - it is a deadlock, not a delay.
  The locks and the discipline are in `fw/src/store.rs`.
- **The calibration lives in its own partition.** Do not move it into `nvs`: a
  Matter factory reset must not force a recalibration, or the other way round.
- **`bat_percent_remaining` is in half-percent units** (0-200 for 0-100%). It is
  easy to get wrong and invisible until a controller shows double the charge.
- **Every wait of the boot loop goes through `Ports::next_event` or
  `Ports::delay`.** That is where the device layer feeds the hardware watchdog.
  A new wait that bypasses both, or a stretch of work longer than
  `tuning::WATCHDOG_TIMEOUT` between them, resets the chip.
- **Every planned end of a boot clears the crashed-boot count**
  (`store::set_crashed_boots(0)`). A new way to end a boot that skips it counts
  as a crash, and three of those in a row make the next boot back off.
- **The battery estimate keeps no state.** Each boot estimates from its own
  reading. Do not add a history, a count or a learned value for it in RTC RAM or
  flash; that was tried and rejected.

## Conventions

- `#![no_std]`, `extern crate alloc`, `alloc` available (`esp-alloc`).
- **Where a number goes is a decision.** Behavioural tuning - cadences,
  thresholds, timeouts, protocol duty cycle - is `core/src/tuning.rs`. Board facts
  - pin map, driver resource bounds - are `fw/src/board.rs`. If a change to one
  needs a change to the other, the number is in the wrong place.
- **`core` names no hardware and no protocol type.** Values cross the boundary as
  plain data in `core::ports`. That is what keeps its tests instant and the
  simulation honest.
- Static allocation via `mk_static!` for anything large or needing a fixed
  address. The Matter stack **must** be static: it needs a stable address and
  would otherwise blow the ~35-50 KiB program stack.
- Errors in async hardware paths are usually logged and dropped rather than
  propagated; the firmware favours staying alive.
- Module-level `//!` doc comments explain *why* - design rationale, spec
  references, hardware quirks. Keep `//` comments for non-obvious reasoning.
- Matter test credentials (VID `0xFFF1`, PID `0x8010`, `TEST_DEV_*`). Not
  certifiable; change only with intent.

## Testing

`make test` runs `core`, `node` and `sim` on the host. A behaviour test belongs in
`sim`, never in a hand-written fake of the boot loop - the simulation runs the
same code the firmware does, so it cannot drift from it.

`docs/testing.md` has the rest, including what is deliberately not covered and
why.
