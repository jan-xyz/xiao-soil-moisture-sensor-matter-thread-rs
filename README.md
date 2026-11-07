# XIAO Soil Moisture Sensor - Matter over Thread (Rust)

A Rust (`no_std`, [embassy](https://embassy.dev/), [esp-hal](https://github.com/esp-rs/esp-hal))
port of [automatous-io/xiao-soil-moisture-sensor-matter-thread](https://github.com/automatous-io/xiao-soil-moisture-sensor-matter-thread),
built on [rs-matter-embassy](https://github.com/sysgrok/rs-matter-embassy)'s
`light_thread_coex` example. Targets the [Seeed Studio XIAO Soil Moisture
Sensor kit](https://www.seeedstudio.com/XIAO-Soil-Sensor-p-6452.html) (a
capacitive soil probe, three status LEDs, a button and an AA battery holder
on a XIAO ESP32-C6 carrier).

> **Disclaimer**: like the original firmware, this uses Matter test
> credentials (vendor ID `0xFFF1`, product ID `0x8010`) and is not
> CSA-certified. Ecosystems will warn about an uncertified device during
> commissioning - that warning is expected. You assume all responsibility
> for any damage, data loss, or device failure.

## Status: pragmatic MVP, not full parity

This is a from-scratch Rust port, not a line-for-line translation, and it
deliberately does not match every feature of the C++ original. See
[Known limitations](#known-limitations) below before flashing.

## Documentation

| | |
|---|---|
| [`ARCHITECTURE.md`](ARCHITECTURE.md) | the layering idea, and where a change belongs |
| [`docs/design.md`](docs/design.md) | how this firmware is put together |
| [`docs/hardware.md`](docs/hardware.md) | the board, and the things about it that bite |
| [`docs/matter.md`](docs/matter.md) | the Matter side, and what the radio costs |
| [`docs/testing.md`](docs/testing.md) | tests, and the simulator |
| [`docs/building.md`](docs/building.md) | building, flashing, and the platform traps |

## Features

- Matter over Thread, BLE-concurrent commissioning (`EmbassyThreadMatterStack`,
  same shape as `rs-matter-embassy`'s `light_thread_coex` example).
- `SoilMeasurement` cluster reporting moisture % (hand-written handler - see
  below).
- Battery-shaped `PowerSource` cluster reporting battery %, voltage and an
  estimated time remaining (hand-written handler - see below). Each boot
  estimates from its own reading, for any kind of AA cell. A fresh cell reads
  full, and towards the end the estimate errs on the empty side - see
  [`docs/battery.md`](docs/battery.md).
- Button: single press samples now, triple click starts the dry/wet
  calibration flow, a long hold factory-resets (erases the Matter fabric and
  reboots). The button also wakes the device from deep sleep.
- Three status LEDs: moisture classification, calibration-flow feedback, and
  error signals - see [Status LEDs](#status-leds).
- Dry/wet calibration persisted to its own flash partition, independent of
  Matter's own fabric/session persistence - a factory reset does not force a
  recalibration, and vice versa.
- Background sampling (on a fixed cadence, and once immediately after boot),
  reporting only when a reading moved enough to matter, so ADC jitter does not
  wake the Thread radio.
- Thread Sleepy End Device with an adaptive duty cycle owned by `rs-matter`'s
  ICD state machine: the receiver is off between data polls, idling at the
  short-idle period or the long-idle one following the controller's ICD
  registration, and polling fast for the active window after boot, any Matter
  message, an ICD stay-active request, a button press, or while commissioning.
  The numbers are in `core/src/tuning.rs`.
- **MCU deep sleep while operating as a LIT**: once the
  device is idle and has taken its reading, the CPU, RAM and every APB-clocked
  peripheral are powered off until the next wake-up is due. Measured on the
  board: ~0.12 mA asleep against ~100 mA awake, so the seconds-long wake -
  not the sleep - is the whole battery story - see [Power](#power).
- LIT ICD: hosts the Matter ICD Management cluster on endpoint 0 through
  `rs-matter`'s `LitIcdMgmtHandler`/`LitIcd`, which sends the Check-In sweep to
  registered clients whose subscription has lapsed, honors `StayActiveRequest`
  and switches between SIT and LIT operation as clients register and unregister.

### Status LEDs

| Signal | Meaning |
|---|---|
| One long blink, red / yellow / green | the reading after a single press: dry / almost dry / normal |
| Slow red blinks | calibration: hold the probe in dry air |
| Slow green blinks | calibration: hold the probe in water |
| Fast yellow blinks | the calibration was rejected; the old references are still in effect |
| Fast red blinks, at the start of a boot | the boot before crashed (a panic, a watchdog reset or a brown-out) |

A calibration is rejected when the dry and wet readings are too close
together, or when the new references cannot be stored. The counts and periods
are in `core/src/tuning.rs`.

### Why hand-written cluster handlers?

`rs-matter` ships no ergonomic handler for `SoilMeasurement`, and its built-in
`PowerSourceHandler` is hardcoded to the *wired* feature. Both are written by
hand here - see [`docs/matter.md`](docs/matter.md).

## Power

The device is a Long Idle Time ICD, and it deep-sleeps between wake-ups. While no
controller has registered for Check-Ins it operates as a Short Idle Time device
instead and **never sleeps**, because it has to stay reachable within seconds.

Each wake must stay up for seconds - long enough to attach to Thread, announce
itself and take a reading. Measured on the board: ~100 mA at the cell while
awake (the Thread radio, the CPU, and the boost converter that makes 3.3 V out
of the 1.5 V cell), ~0.12 mA asleep. The idle period is 6 hours, so a wake
every period draws ~0.05 mA of average against the sleep's 0.12 mA: the
battery story is now the carrier board's sleep floor as much as the wake
cadence, and `make sim` puts a 2500 mAh AA at well over a year.

`make sim` turns that into a battery life from the measured currents, plus the
assumptions it still carries - that number is a *model*, not a measurement.

[`docs/matter.md`](docs/matter.md) has the duty cycle, the table, and what the
model leaves out; [`docs/hardware.md`](docs/hardware.md) has the power path,
what the carrier board contributes, and why the reported battery voltage reads
low.

## Known limitations

Deliberately out of scope for this port (all present in the C++ original):

- **Matter OTA over Thread.** No OTA partitions or requestor.
- **Battery sag / internal-resistance health measurement.** Only resting voltage
  is reported; the LED-load-based health check is not implemented.
- **Brownout diagnostics.**

Consequences of the deep-sleeping LIT profile (see [Power](#power)):

- **A controller that does not implement LIT gets SIT behaviour.** LIT was only
  certifiable from Matter 1.4, and controller support has been slower still, so
  the device falls back to the short-idle polling period and never deep-sleeps.
  Expect the SIT-rate battery life on such a controller, not the LIT one.
- **Two fabrics is a trap.** The device flips to LIT as soon as *either*
  controller registers for Check-Ins, which leaves the other ecosystem unable to
  reach it. See [`docs/matter.md`](docs/matter.md).
- **Commands and OTA are deferred.** The device is unreachable for up to one idle
  period at a time, so anything a controller writes to it lands at the next
  wake-up. That is fine for a sensor and not fine for anything a user expects to
  react to - which is why this is a LIT device and not a SIT one.
- **The idle period is fixed at build time.** One number is both the advertised
  idle interval and the deep-sleep length, and there is no runtime knob - on
  purpose, because the reported scale decays roughly linearly, so a fixed
  interval samples it at uniform resolution and an adaptive one would buy
  nothing but protocol churn. See [`docs/matter.md`](docs/matter.md).
- **The active window is the energy budget.** It has to cover the Thread attach
  and the Check-In exchange on a real network, and it is set by reasoning rather
  than measured on this board.

One constraint discovered during the port, not present in the C++ original:

- **The RNG and the sensors share one ADC unit**, so the firmware seeds its
  CSPRNG once at boot and then releases the ADC for the probe and the battery.
  That is a weaker guarantee than continuous hardware reseeding. The full
  reasoning is in [`docs/hardware.md`](docs/hardware.md).

## Hardware

A Seeed XIAO ESP32-C6 with the soil-moisture carrier: a capacitive probe, three
status LEDs, a button and an AA holder.

The pin map, and the constraints that are easy to break by accident - the
button's GPIO range, the ADC that the radio's entropy source shares, the flash
partitions - are in [`docs/hardware.md`](docs/hardware.md).

## Building and flashing

Requires the `nightly` toolchain with `rust-src` (pinned in
`rust-toolchain.toml`) and [`espflash`](https://github.com/esp-rs/espflash) on
`PATH`.

```sh
make build            # firmware, debug
make build-release    # firmware, release
make flash            # build + flash + monitor
make monitor          # attach to an already-flashed board
make test             # the host test suites
make sim              # run the behaviour against a scripted world, no hardware
make lint             # clippy, rustfmt, unused dependencies, licenses
```

The platform traps - macOS needing a riscv-capable clang, why host commands must
clear `-Zbuild-std`, and why the dependencies are target-gated - are in
[`docs/building.md`](docs/building.md).

### Running it without a board

```sh
make sim ARGS="--days 30"
```

It prints the time spent awake and asleep, the average current, a battery-life
model, and what the device reported. It drives the same boot loop the firmware
runs, against a scripted world whose clock only moves when the code asks it to.
See [`docs/testing.md`](docs/testing.md) for what it can and cannot tell you.

## Commissioning

You'll need a Thread border router (Apple TV/HomePod, Google Nest Hub,
Home Assistant OTBR, or a dedicated RCP) and a Matter controller willing to
commission an uncertified test-credential device. Commission it the same
way you would any Matter-over-Thread accessory; expect an "uncertified
device" warning, which is expected (see the disclaimer above).

## Where things live

The firmware is four crates, split by what each one is allowed to know: the
device's behaviour (no hardware, no protocol, no clock), the Matter node, the
peripherals and wiring, and a simulation of the peripherals. Dependencies point
one way, and the same boot loop runs against the real peripherals and the
simulated ones.

[`docs/design.md`](docs/design.md) has the picture and the reasoning.

## Credits

Firmware behavior ported from
[automatous-io/xiao-soil-moisture-sensor-matter-thread](https://github.com/automatous-io/xiao-soil-moisture-sensor-matter-thread)
(Apache 2.0). Built on [rs-matter](https://github.com/project-chip/rs-matter),
[rs-matter-stack](https://github.com/sysgrok/rs-matter-stack) and
[rs-matter-embassy](https://github.com/sysgrok/rs-matter-embassy).

## License

Licensed under either of

- Apache License, Version 2.0 ([`LICENSE-APACHE`](LICENSE-APACHE))
- MIT license ([`LICENSE-MIT`](LICENSE-MIT))

at your option.

The firmware's behaviour is a port of the C++ original named under Credits,
which is Apache 2.0. This code is a rewrite in Rust, not a copy of its files.
The original has no NOTICE file to carry along.

Every dependency the firmware links is under a permissive license, so the
firmware binary can be passed on under either license above. `deny.toml` lists
the licenses that are allowed in the dependency tree, and `make lint` checks
them.
