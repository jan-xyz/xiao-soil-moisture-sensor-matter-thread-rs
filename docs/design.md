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

A deep sleep is a reboot, so "one boot" is the natural unit of behaviour. A
boot has two phases, split where the protocol stack starts existing:

```rust
pub async fn run_wake<P: Ports>(ports: &mut P, boot: Boot) -> Result<App, Outcome>
pub async fn run_boot<P: MatterPorts>(ports: &mut P, app: App) -> Outcome
```

`run_wake` is the wake phase: the waking press resolved, the boot's one reading
taken. Its port set cannot reach the protocol stack, so this phase cannot
accidentally depend on it - and the composition root (`fw/src/main.rs`, `sim`)
is free to put the stack's startup exactly where it belongs: between the
phases. `run_wake` returns the `App` the operational phase continues from - the
bridge of state across the stack's startup - or the outcome that ends the boot
before the stack ever matters, which is a factory reset asked for through the
wake phase. `run_boot` is the operational phase: the loop that ends in
`Sleep { for_ms }` or `FactoryReset`. Neither phase performs either outcome;
both are irreversible, so `fw/src/main.rs` does, and `sim` performs the sleep
by simply starting another boot.

The wake phase is deliberately dull:

0. on a boot woken by the button, resolve the waking press first: pump the
   gesture state machine until it says whether the press was a single click,
   a triple click, or a hold. Nothing is measured until then, which is what
   makes the gesture's sample and the boot's sample the same one - and the
   reason a triple click does not cost a reading the user never asked for;
1. if a gesture asked for a calibration, run it to completion;
2. take the boot's one reading and report it.

There is no sleep decision here: the protocol stack has not spoken yet, so
there is nothing to sleep from.

The operational loop is deliberately dull:

3. on a boot that is not a wake from sleep, if the reboot count is due, store
   it - the only thing that speaks through the protocol stack's storage, and
   the reason the operational phase's ports are a step up (`MatterPorts`) from
   the wake phase's;
4. if a sample is due, take one and report it;
5. if the world allows it, end the boot by sleeping;
6. otherwise wait for the deadline, a button edge, or a protocol change.

Step 0 exists because the press that wakes the device *is* a user request, and
the boot answers it before the stack has even started: the stack's startup
runs after the wake phase, so the reading lands within a couple of seconds of
the press instead of after the stack's seconds of initialization. The
controller still learns the reading only once the stack is up - the report
goes out with the boot's check-in either way. The phase boundary is what makes
that ordering possible at all: the stack's `startup` takes the stack by `&mut`
- its compile-time rule that `run` must not precede or overlap `startup` - and
the wake phase's ports hold no borrow of the stack, so there is room for the
startup exactly where the phases meet.

Step 1 is a straight line over the ports rather than a state machine, because
nothing else the device does is urgent next to a person standing there holding a
probe. It blocks the boot until it finishes, and the gesture that started it keeps
the protocol stack active for the whole twenty seconds.

Steps 3–6 run in a loop, and that matters: a reading advances the clock, and the
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

Single executor, one RISC-V core. Two tasks run from the very start of the
boot: the button (its press is what may have woken the device) and the status
LEDs (the wake phase's classification has to render immediately). Then the
boot's phases run in sequence in `main` - `run_wake`, then the stack's
`startup`, which takes the stack by `&mut` because `run` must not precede or
overlap it - and after that, two futures under one `select`:

- the Matter stack (`stack.run_coex(...)`, Thread and BLE-concurrent
  commissioning) — the future that terminates on factory reset;
- `run_boot`, the operational phase above.

A factory reset asked for during the wake phase ends the boot before the stack
is ever started, and the wake phase's ports hold no borrow of the stack there,
so the reset's `&mut` meets no objection.

The boot loop and the stack share the flash and the Matter key-value store,
both behind locks whose discipline is that **every flash operation completes
inside the poll of the task that started it** — see `fw/src/store.rs` for why
anything else deadlocks a single-core executor.

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
- **Retained** — the battery trend, the panic flag and the count of crashed
  boots, in RTC RAM. Deep sleep keeps that powered, so it survives a wake-up;
  removing the cell does not, and does not need to.
- **Durable** — in flash. Matter's own fabrics, sessions, subscriptions and
  registrations in the `nvs` partition; the dry/wet calibration in `calib`,
  deliberately separate so that a Matter factory reset does not force a
  recalibration or the other way round.

Anything the protocol can re-derive is deliberately *not* persisted. A
deep-sleep wake resets the dataver, so every subscribed attribute is re-reported
on the next wake-up; trying to suppress that by persisting the last reported
value would buy nothing and could only be wrong.
