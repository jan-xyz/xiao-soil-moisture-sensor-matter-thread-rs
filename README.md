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

## Features

- Matter over Thread, BLE-concurrent commissioning (`EmbassyThreadMatterStack`,
  same shape as `rs-matter-embassy`'s `light_thread_coex` example).
- `SoilMeasurement` cluster reporting moisture % (hand-written handler - see
  below).
- Battery-shaped `PowerSource` cluster reporting battery % and voltage
  (hand-written handler - see below).
- Button: single press samples now, triple click starts the dry/wet
  calibration flow, a 10 s hold factory-resets (erases the Matter fabric and
  reboots).
- Three status LEDs: moisture classification blink (red/yellow/green) and
  calibration-flow feedback.
- Dry/wet calibration persisted to its own flash partition, independent of
  Matter's own fabric/session persistence - a factory reset does not force a
  recalibration, and vice versa.
- Periodic background sampling (every 30 s), reporting only when a reading
  moved enough to matter, so ADC jitter does not wake the Thread radio.
- Thread Sleepy End Device with an adaptive duty cycle: the receiver is off
  between data polls, idling at the SIT period (`THREAD_SIT_POLL_PERIOD_MS`) or
  the longer LIT period (`THREAD_LIT_POLL_PERIOD_MS`) following the controller's
  ICD setting, and shortening to `THREAD_ACTIVE_POLL_PERIOD_MS` for
  `THREAD_ACTIVE_HOLD` after a button press, an ICD stay-active request, or while
  commissioning. Optional CPU light sleep (`--features light-sleep`).
- LIT ICD: hosts the Matter ICD Management cluster on endpoint 0 and
  periodically sweeps registered clients with a Check-In when their subscription
  has lapsed (`CHECK_IN_PERIOD`).

### Why hand-written cluster handlers?

`rs-matter` code-generates the raw attribute plumbing for every cluster in
the bundled Matter IDL, including `SoilMeasurement` (it's `provisional` but
present), but ships no ergonomic handler for it - unlike `OnOff` or
`Descriptor`. Likewise, `rs_matter::dm::clusters::power_source::PowerSourceHandler`
implements only the minimal conformant *wired* shape (`Feature::WIRED`); the
battery attributes always answer `AttributeNotFound`. `src/clusters/`
hand-writes both, following the exact pattern `rs_matter::dm::clusters::desc::DescHandler`
uses: `pub use decl::<cluster>::*` for the generated types, then a
hand-written `impl ClusterHandler`.

## Known limitations

Deliberately out of scope for this port (all present in the C++ original):

- **Matter OTA over Thread.** No OTA partitions or requestor.
- **Battery sag / internal-resistance health measurement.** Only resting
  voltage is reported; the LED-load-based health check is not implemented.
- **Brownout diagnostics.**

One constraint discovered during the port, not present in the C++ original
(ESP-IDF's driver model handles this transparently):

- **The RNG and the sensors share one ADC unit.** The XIAO ESP32-C6 has
  exactly one ADC unit, and `esp-hal`'s `TrngSource` (which supplements the
  hardware RNG with ADC noise for cryptographic-quality randomness) claims
  it exclusively for as long as it's alive - an explicitly
  `#[instability::unstable]` API with a `TODO: a single ADC channel should
  be sufficient` in its own source. This firmware seeds Matter's CSPRNG
  once from true hardware entropy at boot, then releases the ADC for the
  soil probe and battery sensor for the rest of the device's uptime,
  forgoing `rs-matter-stack`'s default periodic HW-entropy reseeding.
  Seed-once-then-run is a standard, broadly accepted practice for embedded
  CSPRNGs, but it is a weaker guarantee than continuous hardware reseeding -
  worth knowing given this device also handles Matter commissioning key
  material. Revisit if/when `esp-hal` narrows `TrngSource` to a single ADC
  channel.

## Hardware

Seeed XIAO ESP32-C6, 4 MB flash. Pin map (`src/pins.rs`, mirrors the
original firmware's `app_priv.h`):

| GPIO | Function |
|---|---|
| 0 | Battery voltage ADC |
| 1 | Soil probe ADC |
| 2 | User button (active low) |
| 3 | RF switch enable (drive low) |
| 14 | Antenna select (drive high, external U.FL) |
| 18 | Yellow LED |
| 19 | Green LED |
| 20 | Red LED |
| 21 | Soil probe excitation PWM |

## Building and flashing

Requires the `nightly` toolchain with `rust-src` (pinned in
`rust-toolchain.toml`) and [`espflash`](https://github.com/esp-rs/espflash)
on `PATH`.

```sh
make build           # cargo build (debug)
make build-release    # cargo build --release
make flash            # build + flash + monitor (debug)
make flash-release    # build + flash + monitor (release)
make monitor           # attach to an already-flashed board
make test              # host-side unit tests (pure calibration/percent math)
make lint               # cargo clippy
make fmt                 # cargo fmt
```

`espflash.toml` points flashing at the custom `partitions.csv` (Matter
state, calibration, and a single non-OTA app partition - see that file for
the layout and rationale).

## Commissioning

You'll need a Thread border router (Apple TV/HomePod, Google Nest Hub,
Home Assistant OTBR, or a dedicated RCP) and a Matter controller willing to
commission an uncertified test-credential device. Commission it the same
way you would any Matter-over-Thread accessory; expect an "uncertified
device" warning, which is expected (see the disclaimer above).

## Credits

Firmware behavior ported from
[automatous-io/xiao-soil-moisture-sensor-matter-thread](https://github.com/automatous-io/xiao-soil-moisture-sensor-matter-thread)
(Apache 2.0). Built on [rs-matter](https://github.com/project-chip/rs-matter),
[rs-matter-stack](https://github.com/sysgrok/rs-matter-stack) and
[rs-matter-embassy](https://github.com/sysgrok/rs-matter-embassy).
