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

## The power path

A single AA cell in the 14500-sized holder feeds a TPS61021A boost converter,
which makes the 3V3 rail (feedback divider R12/R15). Three things about it that
the firmware cannot change:

- **The converter has no firmware authority.** Its EN pin is driven by Q4 from
  VDD_5V: USB present disables the battery boost, and the board runs from USB.
  Deep sleep, factory reset, whatever - the converter keeps the 3V3 rail up for
  as long as the cell is in.
- **Deep sleep is carrier-bound at ~0.12 mA** (measured, multimeter in series
  with the cell). The chip's own share is the datasheet's ~7 µA; the rest is
  the boost's light-load operation and what hangs off 3V3. That is 6-17x the
  chip datasheet, but at the 30-minute wake cadence it is only ~20% of the
  average draw - real, and not the first lever.
- **The awake current is ~100 mA at the cell**, because the boost multiplies:
  ~38 mA at 3.3 V (Thread radio, CPU, everything) comes out of a 1.3-1.4 V
  cell as ~100 mA through the conversion. A millisecond shaved off the awake
  window is worth ~25x a millisecond of deep sleep. And there is nothing to
  underclock: the CPU already runs at esp-hal's default 80 MHz, the lowest
  clock preset the C6 exposes - the 100 mA is radio and boost physics, not
  CPU speed.

## The deep-sleep current budget (0.12 mA measured at the cell)

Deep sleep at the cell measures ~0.12 mA. The chip's own share is the datasheet's
~7 µA (Seeed quotes 15 µA for the whole XIAO at 3.8 V battery input, so the
module's overhead beyond the chip is ~8 µA). At ~1.4 V cell voltage and ~80 %
conversion efficiency, the cell current breaks down roughly as:

- **The boost's light-load operation is the biggest single term.** The TPS61021A
  draws a 17 µA quiescent current *from VOUT* in PFM light-load mode (datasheet),
  plus switching losses delivering the load - about 40 µA at the cell on its own.
  It must run in sleep: EN is pulled to VBAT through R10 (316 kΩ, negligible
  current) and the chip needs 3V3 alive, so nothing can switch it off.
- **The feedback divider R12+R15 (416 kΩ) costs 7.9 µA at 3V3** (~19 µA at the
  cell). It can go multi-MΩ if the converter's FB bias current allows.
- **The button pull.** R6 is 4.7 kΩ to 3V3 on the schematic, which would alone cost
  0.7 mA at 3V3 - the 0.12 mA measurement proves the assembled board does not have
  that fitted. An external 1 MΩ with the internal pull-up disabled costs 3.3 µA
  and still wakes via ext1; the internal pull-up (tens of kΩ) costs tens of µA.
  Make the fitted value explicit in a redesign.
- **R16/C6 battery sense draws nothing standing** (sample cap into a high-Z pad),
  and the LED/probe drivers are off in sleep (base pulled to GND, GPIOs high-Z).

What hangs off the XIAO's 3V3 pin (the carrier back-feeds it): the ESP32-C6FH4 has
flash in package, so there is no external-flash standby current; the charge LED
is on VBUS only; but the **RF switch (FM8625H on the XIAO) is powered from +3V3
through Q3 whenever GPIO3 is not driven high** (R21/R22 hold its gate low), so
drive GPIO3 high before sleeping to cut it. The XIAO's own buck (SGM60629C) has
its output back-driven by our 3V3; its input is diode-isolated (D1) from the 5V
pin, so there is no DC path into the carrier's VDD_5V net.

### Can the board approach the chip's ~7 µA?

Not at the cell with an AA. The cell is 1.2-1.5 V and the chip needs 3.3 V, so a
converter must switch even in sleep; even a 300 nA-Iq boost (MAX1722x class)
delivering ~19 µA at 3.3 V draws ~55-80 µA at the cell once the 3.3/1.4 conversion
ratio and the ~50-60 % light-load efficiency are included. The ~7 µA is only
reachable at the 3.3 V rail (chip plus a few µA of overhead), or at the cell with
a Li-ion cell and a nano-Iq LDO (~8-10 µA). Realistic AA target after swapping
the boost and cleaning up the standing loads: **~60-90 µA at the cell**, roughly
halving today's 0.12 mA. And per the power summary above, a millisecond awake is
worth ~25 ms of sleep, so the awake window remains the first lever.

### The `pcb/` redesign (rev 2, in progress)

The original board was copied into `pcb/pcb.kicad_pcb` (KiCad GUI file copy; the
Copperpour MCP catalogue has no whole-board copy tool, and the skills forbid
writing `.kicad_pcb` directly). Changes made there so far, all via MCP:

- **U1: TPS61021A -> MAX17222ELT+T** (LCSC C1121854, 6-pin UDFN 2x2, JLCPCB in
  stock). 300 nA output quiescent vs 17 µA from VOUT on the TPS61021A; True
  Shutdown (no reverse current into the carrier boost when the XIAO's own buck
  drives 3V3 on USB). Pinout: 1=OUT, 2=LX, 3=GND, 4=SEL, 5=IN, 6=EN.
  - SEL: the MAX17222 has no FB divider - **R15 becomes RSEL = 80.6k to GND for
    3.3 V** (datasheet table). R12 and C9 (the old 416 kΩ divider, 7.9 µA at 3.3 V)
    are deleted.
  - EN: R10 (now **33.2 MΩ**) stays VBAT->EN; the MAX17222's enable-transient
    protection keeps it enabled down to 0.4 V input, and the ETP extra current is
    ~V/R ≈ 45 nA at 33 MΩ (a 316 kΩ pull-up would cost ~4 µA).
- **R6 (button pull-up) = 1 MΩ** (3.3 µA at 3V3 instead of the internal pull-up's
  tens of µA). Requires a firmware change: disable the internal pull-up on GPIO2,
  ext1 wake on the external 1 MΩ still works.
- **R21/R22 deleted** (they would short the cell if fitted; the original had them
  DNP only in the schematic, not in the PCB).
- All values set on the footprints (the original PCB carried footprint names as
  values); U1 carries LCSC/MPN fields.
- Remaining fanout around the new U1 (EN/SEL stubs) is unfinished - a few DRC
  clearance items in the 0.65 mm-pitch pad field are easier to fix in the GUI.
- Firmware also should drive **GPIO3 high before deep sleep** to unpower the
  XIAO's RF switch (Q3 on the module powers it from +3V3 otherwise).
- Layout TODO for µA budgets: guard-ring + soldermask the SEL/EN nodes and the
  R16/C6 battery-sense node, keep pours off them, route sense nets on an inner
  layer, add a 0 Ω jumper in the 3V3 feed to the XIAO to measure its share.

### Layout rules that matter at µA levels

- The FB and EN nodes are high-impedance (416 kΩ / 316 kΩ). Keep VBAT and 3V3
  copper and pours away from them on the same layer, guard-ring them with GND, and
  leave soldermask over them; wash no-clean flux or conformally coat. Surface
  leakage across contaminated FR4/soldermask adds directly to these nodes.
- Same for the battery-sense node (R16/C6/A0_D0, 100 kΩ source) and the probe node
  (CAP_SEN, 1 MΩ to GND): route them on an inner layer (encapsulated in FR4, immune
  to surface contamination) or guard-ringed on an outer layer.
- The 4-layer stack currently uses only F.Cu/B.Cu - 237 tracks, GND pours only on
  F.Cu and B.Cu, In1.Cu ("gnd") and In2.Cu ("power") carry no copper. Either go
  2-layer, or use In1.Cu as a solid GND and In2.Cu for VBAT/3V3 pours so the outer
  layers stay clean around the sense nodes.
- R21/R22 (VBAT- to GND, VBAT_1V5 to VBAT) are DNP in the schematic but carry no
  DNP flag in the PCB; fitted, they would short the cell. Mark them DNP properly
  in a redesign.
- Put a 0 Ω jumper (or test point) in the 3V3 feed to the XIAO, so the module's
  sleep share can be measured separately from the carrier's.

## The battery reading is a loaded reading

The cell voltage reaches GPIO0 through R16 (100K) into C6 (100 nF) - a sample
cap, not a divider, so the path draws no standing current while the chip
sleeps. The price: C6 recharges through the 100K in ~10 ms, which is far
shorter than a wake - so as soon as the board is awake and pulling tens to a
hundred milliamps, the cap reports the *sagged* rail, not the cell's
open-circuit voltage. Expect the reported voltage to sit tens of millivolts
below the true resting voltage for as long as the board is awake - more with
the radio online than in the wake phase, where the sampling happens before the
radios come up. The trend across days therefore mixes true drain with the
wake-load sag, and a multimeter in series with the cell is the better
instrument for answering "how much is really being drawn".

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

`esp_hal::flash::Flash` owns the `FLASH` peripheral and can only be
constructed once, but two independent things need it: Matter's own key-value
store (fabrics, sessions, subscriptions, ICD registrations, the reboot counter)
and the dry/wet calibration. They borrow one instance through a mutex rather than
each claiming their own.

The driver is `esp_hal::flash::Flash` (the in-tree replacement for the
deprecated `esp-storage`): it guards every ROM operation with the flash cache
suspended and the interrupts masked, where the old driver did neither - and,
after the `esp-hal` fork's update, its reads failed outright on this board,
costing the Matter store every persisted key on every boot. The storage users
do not touch the driver directly: each operation constructs a `FlashRegion`
from its `PartitionEntry` (`esp-bootloader-esp-idf`, `embedded-storage`
feature), which bounds-checks against the partition, refuses to touch the
partition the running image lives in, and bounces byte slices into the
word-aligned, DRAM-resident buffers the driver needs. Addresses through these
handles are partition-relative. The price of the guarding is that an erase
keeps the interrupts off for its tens of milliseconds; that is accepted
because erases are rare.

There is no read caching of our own (`fw/src/flash.rs`). Since the KV store
tracks its keys in RAM (upstream rs-matter-embassy PR 91), a lookup for a key
that is not present - of which the fabric table's loader does one per possible
fabric index at every startup, and fabric indices only ever grow - no longer
touches flash at all, and `sequential-storage`'s cache keeps page states and
key pointers in RAM after the first pass. What still walks the flash is that
first pass: the KV store's single key scan per boot and the calibration
lookups, in small sequential guarded ROM reads costing a few tens of
milliseconds per boot. A read-ahead window in RAM used to collapse that walk
into one flash read; it was cut once the upstream cache took over the
repeated-walk cost, leaving only the once-per-boot first pass, which did not
justify the window's code.

The flash path is synchronous down to the driver: every operation completes
inside the poll of the task that started it, and a sector erase blocks the
executor for its tens of milliseconds. That is accepted because erases are
rare - the calibration flow, the KV store's occasional page compaction, and
factory resets - and because the async alternative would only add per-call
overhead to the few reads that remain.

The two live in separate partitions — `nvs` and `calib` — so a Matter factory
reset does not force a recalibration, and recalibrating does not disturb the
fabric. See `partitions.csv` for the layout.

## Nothing resets a hung chip, unless the firmware asks for it

`esp_hal::init` turns every watchdog off. A boot that hangs would then keep the
radio on until the cell is empty, and report nothing on the way. The firmware
arms the RTC watchdog right after `init`, with `tuning::WATCHDOG_TIMEOUT`, and
the device layer feeds it in `Ports::next_event` and `Ports::delay`. Every wait
of the boot loop goes through one of those two, so the watchdog fires exactly
when the loop stops waiting. It is turned off before every deep sleep.

The watchdog only proves that the loop still waits. It does not prove that the
boot ever ends. A protocol stack that wedges in active mode keeps the loop
waiting, and so keeps the watchdog fed. For that case the decision layer caps a
boot in Long Idle Time operation at `tuning::LIT_BOOT_MAX`, and then sleeps for
a full idle period. A short idle device has to stay reachable, so the cap does
not apply to it.

A crash that repeats in a boot's startup, for example on a flash error the
protocol stack cannot get past, would still be a loop: panic, reset, the same
crash. The count of boots in a row that did not reach a planned end lives in RTC
RAM. Past `tuning::CRASH_LOOP_BOOTS` the next boot sleeps for
an idle period before it tries again (`soil_sensor_core::recovery`). A button
wake always starts, so a factory reset stays possible.

A reset over USB starts the count from zero. Flashing the firmware or attaching
a monitor resets the chip that way, and the boot it cuts short never reaches a
planned end. Without this rule, every flash would look like a crash, and RTC RAM
left over from another firmware could look like a crash loop.

Nothing else makes a crash visible: the serial console is gone after the first
deep sleep, and the controller only sees a device that went quiet. So a boot
that follows a crashed one starts with fast red blinks
(`LedCommand::crashed`). The README lists every LED signal.

## The status LED driver runs at 6-bit duty resolution

The LEDC divisor formula undershoots its valid range at 200 kHz with 8-bit
resolution unless the APB clock is exactly 80 MHz. Six bits stays valid down to
40 MHz and is far finer than the two duty values the probe excitation ever asks
for.

## A sleepy end device receives its downlink in bursts

Frames arrive in a burst after each data poll, which overflows esp-radio's default
802.15.4 receive queue — logged as `Receive queue full`. The queue depth is raised
in `fw/src/board.rs`. The queue is one heap block of frames that doubles as it
fills and is never given back until reset. That is why the depth is a power of
two: any other depth rounds up to the next power of two, and that block is
allocated inside the receive interrupt while the old block is still held for the
copy.

## The EUI-64 must be the same on every boot

Thread's SRP client registers services under identifiers derived from the device's
EUI-64. A random one per boot makes the same logical service look like a new host
to the SRP server, which rejects it as a name conflict — and the device shows up
as "Offline" in Home Assistant after every reboot.

It is therefore taken from the factory-programmed MAC, the way ESP-IDF does it:
the base EUI-48 plus the `MAC_EXT` eFuse bytes.
