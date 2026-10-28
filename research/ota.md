# Matter OTA updates: findings, design, and how we retire the unknowns

This document is the research that preceded any OTA code. It exists so that the
work starts from evidence rather than from re-exploration, and so that the
decision to build it - or not to - can be reviewed before anything is wired to
hardware.

It is a snapshot of the pinned dependency set in `Cargo.toml` at the time of
writing. The claims about `rs-matter` and `esp-bootloader-esp-idf` were checked
against those exact revisions; moving a pin invalidates them.

## Goal and scope

The device is commissioned through a Matter ecosystem and should receive
firmware updates the way every other Matter device does: the controller (or its
border router) acts as the **OTA Provider**, the device acts as the **OTA
Requestor**, and the image arrives over the existing Matter session as a BDX
transfer. No USB, no serial cable, no custom protocol.

A custom Thread/UDP uploader was considered and rejected. It would duplicate
transport code the protocol stack already has, it would not work through any
controller a person actually owns, and `ARCHITECTURE.md` assigns protocol
semantics to the integration layer - a bespoke uploader is protocol semantics by
definition. If Matter OTA proves infeasible on this hardware, the fallback is
*not* a custom protocol; it is the physical flash path that exists today.

Out of scope: device-side OTA *Provider* (this device will never serve images),
image signing infrastructure (the device uses Matter's test credentials and is
not certifiable; see `README.md`), and delta updates (Matter 1.4's
`ApplyUpdateAction::Proceed` path is the simple one).

## What already exists

Both hard mechanisms are present in the pinned revisions. The work is glue, not
infrastructure.

### `rs-matter` at `f5b40c4`

The OTA Requestor building blocks live in `dm::clusters::ota_req`, reachable
from the firmware through `rs_matter_embassy::matter::...`:

| Building block | What it gives |
|---|---|
| `Providers` | The persistent, fabric-scoped `DefaultOTAProviders` list (one entry per fabric) plus a transient cache of providers learned via `AnnounceOTAProvider`. Persisted by `rs-matter` itself, into the same KV store as the fabrics. `wait_changed` lets the app react. |
| `Provider::query` | A CASE exchange to one provider carrying `QueryImage`, with the response handed to a callback before the buffer is released. It does not interpret the response; the app checks status and version. |
| `OtaState` | The update state the cluster reports, with an RAII session (`initiate_update`) that emits the `StateTransition` and `DownloadError` events; `VersionApplied` is emitted at startup when the running version differs from the last. |
| `bdx::BdxDownloadInitiator` | The BDX transfer itself, driven from an exchange. |
| `parse_bdx_url` | Turns the provider's `bdx://` image URI into a `(node, file-designator)` pair. |
| `OtaImageHeader::load` | Parses the Matter OTA image header (vendor, product, version) off the downloaded image. |

`rs-matter`'s own integration tests exercise both roles against CHIP's
`chip-ota-{provider,requestor}-app`, so the protocol mechanics are tested
upstream. What upstream does not test is a sleepy end device on real Thread
hardware - which is where the risk in this project concentrates.

### `esp-bootloader-esp-idf` in the pinned `esp-hal` fork

`ota.rs` and `ota_updater.rs` implement the ESP-IDF two-slot scheme on top of
`esp_storage`: an `otadata` partition recording the selected slot, `OtaUpdater`
for "write the image into whichever slot is not booted, then select it", and
`OtaImageState` (`New` / `PendingVerify` / `Valid` / `Invalid`) with the
bootloader-side rollback that goes with it. It rejects a table with fewer than
two OTA app slots - the current `partitions.csv` cannot host it.

### What does not exist

`rs-matter-embassy` provides no OTA wiring; its README lists "Matter OTA
support" under *Future*. Everything between "provider announced" and "slot
written and rebooted" is this codebase's work - which is where it belongs:
mechanism in `node`/`fw`, policy in `core`.

## Constraints the design must survive

### The flash is full of things that must not move

4 MB of flash today: `nvs` (Matter's KV store, 24 KiB), `calib` (the
calibration FAT partition), and one `factory` app partition taking the rest
(`partitions.csv` was written with "no OTA slots, out of scope for v1" as a
comment). The OTA layout needs an `otadata` partition and two app slots. Two
slots of roughly 1.9 MB each fit comfortably, but the per-image headroom halves
compared with today's single 3.9 MB partition. `make size` currently reports
against `0x3E0000`; its constant must follow the new slot size.

Whether `factory` survives as a rescue image or the device becomes two
interchangeable slots is a decision Phase 0 must make; the bootloader treats a
`factory` partition as the default when `otadata` is empty, which is a useful
property to keep or deliberately give up.

### The bootloader must actually do OTA

`esp-bootloader-esp-idf`'s own documentation carries the warning that matters:
*the prebuilt bootloaders provided by `espflash` might not include OTA
support*. If the second-stage bootloader currently on the boards does not read
`otadata`, changing the selected slot does nothing and every later phase is
built on sand. This is the first thing to test and the most likely source of
lost days; it may require building the ESP-IDF second-stage bootloader (a C
build) and documenting it in `docs/building.md`.

### The device sleeps by rebooting

The sleep model is deep sleep, and a wake is a full reboot (`docs/matter.md`).
Three consequences for OTA:

1. **An OTA must hold the device awake for its whole duration.** The decision
   layer's sleep gate (`App::sleep_for`) gains a condition: no sleep while an
   update is in progress. The existing gate set (LIT operating mode, stack
   idle, no gesture, settle delay elapsed) shows the shape.
2. **An interrupted update leaves a half-written slot.** The download state -
   at minimum the BDX update token and the byte offset - must be durable, in
   the same sense the fabrics and the calibration are, so a reboot resumes
   rather than restarts. BDX supports resumption; the token just has to
   outlive the reboot.
3. **The cost is real and one-sided.** A ~1.5 MB image over Thread at the 300 ms
   active poll period is a rough estimate of ten to twenty minutes of the
   device awake at its active current - a few mAh, once per update. That is
   acceptable per event and invisible until the energy model in `sim/` learns
   about it. The number here is a *model*, not a measurement; the simulator's
   honest-limits note applies.

The keep-awake must be provider-agnostic. The multi-fabric hazard documented in
`docs/matter.md` applies in force: the provider may sit on a different fabric
than the one holding the device awake, and the gate cannot assume they are the
same.

### The flash bus has a contract

All flash access goes through `fw/src/flash.rs`'s `SharedFlashBus`, whose
discipline is documented and load-bearing: operations complete inside the poll
of the task that started them, no await holds the lock, and an erase blocks the
executor for tens of milliseconds (up to ~100 ms for a full sector).

An OTA download writes megabytes. Erasing a 1.9 MB slot in one call would
freeze the executor for on the order of two seconds - long enough to wreck
radio timing in the middle of a live BDX exchange. The OTA writer must erase in
chunks (roughly 64 KB at a time) with awaits between them, so the freezes stay
within the tolerance the Matter stack already lives with. Writing flash while
executing from it is not a concern: `esp_storage` goes through the ROM, which
suspends the cache, and the KV store already writes flash on this device
routinely.

### The version has to be real

`BasicInfoConfig` currently inherits `TEST_DEV_DET`'s static software version.
`Provider::query` compares the running version against the provider's offer
using the values from the Basic Information cluster. Until the real
application-descriptor version (from `esp_app_desc!`, currently the workspace's
`0.1.0`) is wired into that cluster - and until a release process bumps it -
no provider will ever offer an update, and the device will look broken while
being merely unversioned.

## The proposed plan

Five phases, each with the test story the `ARCHITECTURE.md` table demands.
The phase boundaries are the points where the work is independently useful.

### Phase 0 - Groundwork (about half a day)

- Rewrite `partitions.csv`: keep `nvs` and `calib` unchanged; replace the
  single `factory` app with `otadata` (0x2000), `ota_0` and `ota_1`, sized to
  split the remainder. Decide the `factory` question deliberately.
- Verify or build an OTA-capable second-stage bootloader. Update
  `espflash.toml`, `scripts/flash-and-monitor.sh`, and `docs/building.md` with
  what was needed - this is the phase whose findings rewrite the docs.
- Point `scripts/firmware-size.py`'s `APP_PARTITION_BYTES` at the new slot
  size.
- Wire the application-descriptor version into `BasicInfoConfig`'s
  `software_version` / `software_version_string`, and write down the rule:
  an OTA release is a version bump, or nothing will ever update.

**Test story:** build; slot selection verified on hardware (see the
unknowns table). No behaviour change otherwise.

### Phase 1 - The flash mechanism, behind a port (one to two days)

- A new `Ota` capability in `fw/src/ports.rs`, implemented by a new
  `fw/src/ota.rs` wrapping `OtaUpdater` over the `SharedFlashBus`: enumerate
  slots, stream-write into the inactive slot with chunked erases, finalize
  (validate, select), and read/mark image state at boot.
- Boot-time confirmation policy in the decision layer: the first boot into a
  new image marks it valid only if the boot is clean - the panic-flag pattern
  in `fw/src/store.rs` (set from `custom_halt`, taken at boot) is the exact
  precedent. A boot that panics before confirming lets the bootloader roll
  back.
- The simulation implements the same port and gains a scenario where an
  interrupted update survives a restart and one where a panicking boot rolls
  back.

**Test story:** decision-layer cases for the confirmation policy;
simulation across restarts; the mechanism itself proven on hardware.

### Phase 2 - The Matter integration (two to four days)

- `node/src/ota.rs`, following the existing handler recipe in
  `node/src/power_source.rs`: host the `OtaSoftwareUpdateRequestor` cluster on
  endpoint 0, backed by rs-matter's `Providers` (its persistence is already
  handled). Endpoint 0 needs the spelled-out `clusters!(thread ; ...)` form,
  and the catch-all matcher must be chained first so it is evaluated last -
  the ordering note in `docs/matter.md` is load-bearing.
- `fw/`: an OTA state machine task. On provider announcement or
  `Providers::wait_changed`, run `Provider::query`, `parse_bdx_url`, stream
  `Exchange::download` into the `Ota` port, validate with
  `OtaImageHeader::load` (vendor, product, version), report progress through
  `OtaState::initiate_update`, and reboot to apply.
- Keep-awake as a decision-layer event, not a task-side flag: the download's
  progress is an event the boot loop consumes, and the sleep gate refuses
  while it is active. The LED task reports progress and failure states, as it
  does for gestures and calibration.
- Persist the BDX update token durably so an interrupted download resumes
  after a reboot.
- A battery floor in `core/src/tuning.rs`: below it, an OTA is refused or
  aborted - a device that dies mid-update has burned its charge for nothing.

**Test story:** protocol behaviour against the stack; the state machine in
simulation; the transfer itself on hardware.

### Phase 3 - Decision layer and simulation (one to two days)

- `core/`: OTA events and actions as plain data through `core::ports` -
  announced, query ready, download progress, apply-ready - with no hardware
  or protocol types crossing the boundary. Tuning values: maximum download
  duration, retry/backoff, the battery floor.
- `sim/`: the energy model gains a "stayed awake for the whole download"
  mode; the scenarios from Phase 1 (resume, rollback) join the behaviour
  suite.

**Test story:** the cadence, the budget, and the lifetime questions become
ordinary assertions - which is what the simulation exists for.

### Phase 4 - End to end, and packaging (one to two days)

- Release tooling: wrap the release binary in a Matter OTA image (rs-matter's
  `xtask ota` builds and inspects these) so a provider can serve it. The
  payload inside the Matter header is the raw ESP application image.
- End to end against a real provider - Home Assistant, or CHIP's
  `chip-ota-provider-app` for a controlled pairing: announce, query,
  download, apply, confirm, and the sleepy-device interactions around all of
  it.
- Docs: the OTA state machine and its energy cost in `docs/matter.md`; the
  bootloader and version-bumping story in `docs/building.md`; the
  where-a-change-belongs rows in `ARCHITECTURE.md`.

## Unknowns, and how to eliminate them

Each unknown below names the cheapest experiment that settles it and the
fallback if the answer is bad. They are ordered so that the ones that can kill
the project come first; a weekend of experiments retires the whole table.

| # | Unknown | Why it matters | Cheapest experiment | Fallback |
|---|---|---|---|---|
| 1 | Does the flashed bootloader read `otadata`? | Without it, slot selection is a no-op and the whole design rests on nothing. | Flash a table with two slots, deliberately mark the alternate slot in `otadata` via a test build, watch the boot choice on serial. Half a day. | Build the ESP-IDF second-stage bootloader (C toolchain); document it in `docs/building.md`. |
| 2 | What BDX throughput does a 300 ms poll actually deliver? | Sets the awake duration, the energy cost, and the timeout tuning values. Everything downstream is sized by it. | Time a controlled transfer against `chip-ota-provider-app` - rs-matter's integration tests pair exactly these two. Measure, do not model. | Raise the poll period during an OTA only; the battery cost of a faster poll for twenty minutes is noise next to being awake at all. |
| 3 | Do real providers tolerate a sleepy device's transfer pace? | A provider that times out mid-transfer turns every update into a resume story. | The same experiment as #2; repeat with Home Assistant's provider, which is the ecosystem this device is aimed at. | Graceful abort plus durable resume (already in the design); tune the BDX block size the requestor advertises. |
| 4 | Does the current image fit the slot with headroom? | If it barely fits, growth becomes a release blocker. | `make size` against the future slot size - arithmetic, no hardware. | Shrink: `opt-level = "s"` is already set; the Matter stack dominates and the fork set is fixed, so trimming is limited - reconsider the slot split instead. |
| 5 | Do BDX buffers fit the 110 KiB heap? | A download that cannot buffer is a design failure discovered late. | Build with the OTA code enabled and read the size report; stress the buffer path in simulation first. | Shrink `BUMP_SIZE`/buffers or negotiate a smaller BDX block size. |
| 6 | Does a chunked erase stay inside the radio's timing tolerance? | A two-second executor freeze mid-exchange would be the deadlock class the flash docs warn about. | Profile one 64 KB chunk erase under an active exchange on hardware; watch for lost polls. | Smaller chunks; or accept erases between exchanges rather than during. |
| 7 | Does rs-matter's BDX initiator negotiate block sizes a SED can sustain? | Upstream tested it between hosts; a sleepy peer is a different animal. | Read the negotiation code; confirm against the #2 measurement. | Patch the fork - the fork set already exists for exactly this kind of gap. |

## Decision record

The work proceeds when unknowns #1 and #2 have honest answers, because every
phase after Phase 0 is sized by them. The work stops - and the device keeps
its USB-flash update path - if:

- the bootloader cannot be made to select slots (unknown #1's fallback fails),
- or real providers time out against a sleepy transfer with no way to resume
  (unknown #3's fallback fails),

because at that point the standard path costs more than the problem justifies,
and the honest alternative remains the cable.
