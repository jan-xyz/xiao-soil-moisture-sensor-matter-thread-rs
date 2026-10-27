# How the firmware is put together

This is the project's own architecture. `../ARCHITECTURE.md` explains the
*layering idea* in general terms and is deliberately free of project detail; this
page is where the idea lands, with the names and numbers this device actually
uses. Read the other one first if you want to know why any of this is arranged
the way it is.

## Four crates, and one direction

```
   core/    the decision layer     what the device does
            no hardware, no protocol, no clock, no executor
              |
   node/    the integration layer  how it speaks Matter
            the protocol, still no hardware
              |
   fw/      the device layer       the peripherals, and the wiring
            plus the composition root, which is the only place that knows all of them

   sim/     the simulation layer   a peer of fw/, host-only
            drives the same boot loop against a scripted world
```

Dependencies point one way. `core` names no peripheral and no protocol type;
`node` names the protocol but no peripheral; `fw` names both and decides nothing.
`sim` implements the same ports `fw` does, so the behaviour tests run the code the
device runs rather than a copy of it.

The split between `core` and `node` is not cosmetic. A crate that depends on
`rs-matter` cannot be *linked* on the host without a time driver and a
`critical-section` implementation, both of which only arrive with `rs-matter`'s
`os` feature (which pulls in `std`). Keeping `rs-matter` out of `core` is what
lets `core`'s tests link against nothing and finish in milliseconds.

## One boot, from power-up to whatever ends it

A deep sleep is a reboot, so "one boot" is the natural unit of behaviour.
`core::app::run_boot` drives exactly one and returns how it ended:

```rust
pub async fn run_boot<P: Ports>(ports: &mut P, boot: Boot) -> Outcome
```

`Outcome` is either `Sleep { for_ms }` or `FactoryReset`. The boot loop does not
perform either, because both are irreversible; `fw/src/main.rs` does, and `sim`
performs the first by simply starting another boot.

The loop is deliberately dull:

1. if a gesture asked for a calibration, run it to completion;
2. if a sample is due, take one and report it;
3. on a boot that is not a wake from sleep, if the reboot count is due, store
   it;
4. if the world allows it, end the boot by sleeping;
5. otherwise wait for the deadline, a button edge, or a protocol change.

Step 1 is a straight line over the ports rather than a state machine, because
nothing else the device does is urgent next to a person standing there holding a
probe. It blocks the boot until it finishes, and the gesture that started it keeps
the protocol stack active for the whole twenty seconds.

Steps 1–3 run in a loop, and that matters: a reading advances the clock, and the
advance can carry the device past the moment its protocol stack went idle.
Doing one thing and then waiting would leave it sampling every thirty seconds
forever instead of noticing it may sleep. This was a real bug, found by the
simulator on its first run.

## The ports

`core::ports::Ports` is the only vocabulary the layers share, and it is
implemented exactly twice: `EspPorts` over real peripherals, `SimPorts` over a
scripted world. Ports are named after what the layer above needs ("read the
ambient value"), never after what the layer below is ("read this ADC channel").

Two of them exist to answer questions the decision layer must never ask itself:
`now()` — what time is it — and `next_event(deadline)` — which of the things I am
waiting for happened first. Putting the second one in the port is deliberate:
deciding whether a deadline, a button edge or a protocol change comes first is a
property of the world, not a decision, and it is exactly where a simulator has to
behave like hardware.

`next_event` must always be called with a deadline strictly in the future.
`App::next_deadline` guarantees that, and the simulator asserts it, because a
deadline that has already passed is a wait nothing can end.

## The task graph

Single executor, one RISC-V core, three futures under one `select3`:

- the Matter stack (`stack.run_coex(...)`, Thread and BLE-concurrent
  commissioning) — the future that terminates on factory reset;
- `run_boot`, the boot loop above;
- a `join` of the two tasks that must not miss an event while the boot loop is
  busy: the button (edges into a channel, and the deep-sleep wake source) and the
  status LEDs (a blink outlives the moment that asked for it).

It used to be eight futures sharing four channels and two `AtomicBool`s. All of
that state is now fields of `App`, which is why the boot loop can be tested at
all.

## Where a change belongs

`../ARCHITECTURE.md` has the general table. For this device:

| Change | Where |
|---|---|
| A cadence, threshold, timeout, or the protocol duty cycle | `core/src/tuning.rs` |
| A pin, or a driver resource bound | `fw/src/board.rs` |
| A new user gesture | `core/src/button.rs`, with a case in its tests |
| A new sensor | a method on `Ports`, plus `EspPorts` and `SimPorts` |
| A cluster attribute or command | `node/` |
| A new value that must outlive a reboot | `core` decides its lifetime; `fw/src/store.rs` provides the storage |

If a change to one needs a change to the other, the number is in the wrong place.

## What survives what

Three lifetimes, and the decision layer is where each one is declared:

- **Volatile** — the boot itself. Most of `App`.
- **Retained** — the battery trend, in RTC RAM. Deep sleep keeps that powered,
  so it survives a wake-up; removing the cell does not, and does not need to.
- **Durable** — in flash. Matter's own fabrics, sessions, subscriptions and
  registrations in the `nvs` partition; the dry/wet calibration in `calib`,
  deliberately separate so that a Matter factory reset does not force a
  recalibration or the other way round.

Anything the protocol can re-derive is deliberately *not* persisted. A
deep-sleep wake resets the dataver, so every subscribed attribute is re-reported
on the next wake-up; trying to suppress that by persisting the last reported
value would buy nothing and could only be wrong.
