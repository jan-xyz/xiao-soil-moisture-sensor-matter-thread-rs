# The board, and the things about it that bite

A Seeed XIAO Soil Moisture Sensor kit: a capacitive probe, three status LEDs, a
button and an AA holder, all on a XIAO ESP32-C6 carrier with 4 MB of flash.

Most of this page is a list of constraints that look arbitrary and are not. Each
one has a reason, and each one has already cost somebody an afternoon.

## Pin map

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

## The radio's random number generator and the sensors share one ADC

`esp-hal`'s `TrngSource` supplements the hardware RNG with ADC noise, which means
it claims ADC1 exclusively for as long as it lives — and this board has exactly
one ADC unit, which the probe and the battery sense both need. It is an
`#[instability::unstable]` API with a `TODO: a single ADC channel should be
sufficient` in its own source, so this is a known limitation rather than a
stable guarantee.

The firmware therefore seeds a software CSPRNG (`ChaCha12Rng`) from true hardware
entropy once at boot and then drops `TrngSource` to free the ADC for the rest of
the device's life.

`reseeding_csprng` is deliberately **not** used. It stores a `Trng` for the
process lifetime so it can reseed periodically, which means `TrngSource` — and
therefore ADC1 — could never be released. Dropping `TrngSource` while a `Trng` is
alive panics, so the two are mutually exclusive.

Seed-once-then-run is normal practice for embedded CSPRNGs, but it is a weaker
guarantee than continuous hardware reseeding, and worth knowing given that this
device also handles Matter commissioning key material.

## The button is on GPIO2, and cannot move

Deep sleep powers down the high-performance GPIO peripheral, so a wake source has
to be one of GPIO0–7 — the pads the chip's `ext1` low-power path reaches. The
XIAO's BOOT button is GPIO9 and therefore cannot wake this firmware. Moving the
user button there would silently cost the device its only input.

The wake itself needs no special arming: `apply_wakeup_config` marks the pad once
at startup, and the pin's own interrupt trigger is the wake condition — the
`wait_for_falling_edge` the button task is already sitting on is what arms the low
level. A pin that is not listening is not a wake source, which is also why the
button task exists at all rather than the boot loop polling the pin - and why it
is spawned at the very start of the boot, before the Matter stack takes its
seconds to come up. A release that lands inside that startup would otherwise
never be seen.

The wake cause needs care too: on this chip the low-power pad's wake is
reported as `Ext1`, not as `Gpio` - the pad's wake path is the one `LP_AON`
drives, and the wake-cause bits follow it. Classifying a wake as the button's
has to accept both names, or every button wake reads as a timer wake and the
gesture state machine never starts mid-gesture at all.

Because the pin is pulled up, the pull resistor is what holds it at the level that
does *not* wake the chip. A pad with no pull would float, and a floating pad wakes
the chip immediately and repeatedly.

## A wake-up reboots with the button still down

A deep-sleep wake is a reboot that the button caused, so the firmware comes back
up with the press still in progress and no falling edge pending. The gesture state
machine therefore starts *mid-gesture* rather than waiting for an edge that will
never come, and the boot widens its active window so the gesture it is part of —
a triple click, or the ten-second factory-reset hold — has room to finish.

If the waking press has already ended by the time the pin can be read - a quick
tap, which is the ordinary case - `main` hands the state machine the release it
never saw, so the tap still counts as a tap. Without that, a tap looks like a
press that never ends: no sample, no LED, and a factory reset when the hold
deadline lapses ten seconds later.

Without both halves, the factory-reset hold can never complete on a device that
sleeps.

## Flash is one peripheral with two users, and one page of read-ahead

`esp_storage::FlashStorage` owns the `FLASH` peripheral and can only be
constructed once, but two independent things need it: Matter's own key-value
store (fabrics, sessions, subscriptions, ICD registrations, the reboot counter)
and the dry/wet calibration. They borrow one instance through a mutex rather than
each claiming their own.

Every read also serves from a read-ahead window in RAM (`fw/src/flash.rs`),
sized past the largest partition the firmware scans. The KV store's lookups
walk stored items' headers in small sequential reads, and a lookup for a key
that is not present - of which the fabric table's loader does one per possible
fabric index at every startup, and fabric indices only ever grow - re-walks
the same region hundreds of times. An earlier, one-page buffer did nothing
here: a walk covers six flash pages, so a one-page cache over that working
set re-read the whole region on every walk. With the whole region buffered,
the first walk costs one real flash read and every further walk is pure RAM.
The buffer is invalidated on every write and erase, and it lives under the
same lock as the flash, so a scanner can never hold a stale window across
someone else's write.

That wrapper is `YieldingAsync`, not plain `BlockingAsync`, and this matters: a
sector erase blocks for tens of milliseconds, and on a single-core executor with
BLE and the Thread radio sharing it, a non-yielding erase would freeze everything
for the duration.

The two live in separate partitions — `nvs` and `calib` — so a Matter factory
reset does not force a recalibration, and recalibrating does not disturb the
fabric. See `partitions.csv` for the layout.

## The battery trend lives in RTC RAM

Deep sleep keeps the always-on domain powered, so RTC RAM survives a wake-up —
and since a wake-up is a reboot, that is the only way a battery trend can
accumulate across one. A trend in ordinary RAM would restart from nothing every
fifteen minutes and could never span the four hours an estimate needs.

The storage sits in a `NOLOAD` linker section, which means the initialiser never
runs and the memory holds whatever was there before. The code therefore gates on
a magic value and rebuilds the trend when it does not match. Bump that magic
whenever the layout changes, or a stale image will be read as if it were current.

The trend type itself is plain data — parallel arrays, no `Option`, no tuples, no
padding — so that it can be placed there at all, and it clamps its own indices so
that a torn image degrades to a wrong estimate rather than an out-of-bounds read.

## Two clocks, and they are not interchangeable

- **Boot-relative** (`Ports::now`): milliseconds since this boot began. Used for
  every deadline inside a boot. It restarts at zero on every wake-up.
- **Since power-on** (`Ports::uptime_s`, from the RTC): keeps counting across the
  reboots a deep sleep ends in. Anything that has to stay ordered across wake-ups
  — the battery trend — is timestamped with this one.

Using the wrong one is a silent bug: the trend's samples would go backwards on
every wake-up, and the least-squares fit would quietly produce nonsense.

## The status LED driver runs at 6-bit duty resolution

The LEDC divisor formula undershoots its valid range at 200 kHz with 8-bit
resolution unless the APB clock is exactly 80 MHz. Six bits stays valid down to
40 MHz and is far finer than the two duty values the probe excitation ever asks
for (0% and 68%).

## A sleepy end device receives its downlink in bursts

Frames arrive in a burst after each data poll, which overflows esp-radio's default
802.15.4 receive queue — logged as `Receive queue full`. The queue depth is raised
to 200 in `fw/src/board.rs`. Each queued frame is a ~130-byte heap buffer held
until reset, so that depth caps the worst case at roughly 26 KB.

## The EUI-64 must be the same on every boot

Thread's SRP client registers services under identifiers derived from the device's
EUI-64. A random one per boot makes the same logical service look like a new host
to the SRP server, which rejects it as a name conflict — and the device shows up
as "Offline" in Home Assistant after every reboot.

It is therefore taken from the factory-programmed MAC, the way ESP-IDF does it:
the base EUI-48 plus the `MAC_EXT` eFuse bytes.
